//! "Download missing media" (`media_backfill`): the art `media_download` enables that linked
//! releases don't have yet -- a type just switched on, a download that failed, a release
//! linked by a sync that skips art. Run by hand or on a schedule, over owned releases or the
//! whole catalogue.
//!
//! First of all it puts back art that is on record but not on disk -- a database restored
//! without its media, a media dir emptied or replaced ([`restore_missing_files`]). Such art
//! is "held" as far as every gate below can tell, so nothing else would ever fetch it again.
//!
//! Only entries already linked to a provider are looked at; nothing is searched for by name.
//! Each `(provider, id)` is resolved once for all the releases it covers, through
//! `providers::media::attach_for` -- the same per-target gate a page open uses, so a release
//! with a manual pick or a higher-ranked provider's art is left alone.

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::Row;

use crate::database::jobs::Progress;
use crate::database::providers::igdb_localized;
use crate::database::providers::media::{self, Attached, MediaTarget};
use crate::database::providers::raw_store::RawStore;
use crate::database::providers::thegamesdb_media as tgdb_media;
use crate::database::providers::xlenore;
use crate::database::providers::Provider;
use crate::database::settings;

use super::{App, Error, Result};

pub use romexplorer_types::jobs::MediaBackfillRequest;

#[derive(Debug, Clone, Default, Serialize)]
pub struct MediaBackfillSummary {
    pub scope: String,
    /// Releases in scope that have at least one provider link.
    pub releases: u64,
    /// Provider entries looked at (one per linked `(provider, id)`).
    pub entries: u64,
    /// Links made to downloaded (or already stored) files.
    pub downloaded: u64,
    /// Files put back for art that was on record but missing on disk.
    pub restored: u64,
    /// Entries whose releases already had everything asked for.
    pub already: u64,
    /// Entries skipped because their provider has no credentials and nothing cached, by provider.
    pub unavailable: BTreeMap<String, u64>,
    pub errors: Vec<String>,
    /// Why the run stopped short: a provider's allowance ran out (run it again next month and
    /// it goes on from where it stopped), or nothing asked for was linked to the provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    /// Releases linked to TheGamesDB by this run, from its free dump (no API call).
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub linked: u64,
    /// PS1 / PS2 releases in scope looked up by serial (xlenore's covers).
    #[serde(skip_serializing_if = "is_zero_u64")]
    pub serial_releases: u64,
    /// Provider API calls spent on image lists (TheGamesDB's count against its allowance).
    #[serde(skip_serializing_if = "is_zero")]
    pub list_calls: u32,
    /// A client asks its main server instead (`media_sync`); these are its counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via_server: Option<super::media_sync::MediaSyncSummary>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

pub fn validate(request: &MediaBackfillRequest) -> Result<()> {
    if !matches!(request.scope.as_str(), "owned" | "all") {
        return Err(Error::invalid("scope must be owned or all"));
    }
    if request.selection.as_ref().is_some_and(|s| s.is_empty()) {
        return Err(Error::invalid("the selection is empty"));
    }
    for name in &request.providers {
        if name == xlenore::NAME {
            continue;
        }
        let Some(provider) = Provider::from_name(name) else { return Err(Error::invalid(format!("unknown provider {name:?}"))) };
        if media::provider_media(provider).is_empty() {
            return Err(Error::invalid(format!("{name} has no art to download")));
        }
        if provider.media_manual_only() && !request.is_asked() {
            return Err(Error::invalid(format!("{name} art is downloaded only for the types you choose")));
        }
    }
    for slot in &request.types {
        let (media_type, sub) = match slot.split_once(':') {
            Some((t, s)) => (t, Some(s)),
            None => (slot.as_str(), None),
        };
        if !super::settings::MEDIA_TYPES.contains(&media_type) || sub.is_some_and(|s| !crate::database::media_store::SUB_CATEGORIES.contains(&s)) {
            return Err(Error::invalid(format!("unknown art type {slot:?}")));
        }
    }
    Ok(())
}

/// [`validate`], and what only this machine can say: a provider asked for by name has its
/// credentials here, and a narrowed run is not on a client (whose art comes from its server).
/// Checked when the job is queued, so the answer comes at once rather than as a job that
/// finished having done nothing.
pub async fn validate_with(app: &App, request: &MediaBackfillRequest) -> Result<()> {
    validate(request)?;
    let narrowed = request.selection.is_some() || !request.providers.is_empty() || request.is_asked();
    if narrowed && settings::instance_mode(&app.pool).await? == settings::InstanceMode::Client {
        return Err(Error::invalid("download art for a selection on the main server; this machine takes its art from there"));
    }
    let clients = app.clients();
    for name in &request.providers {
        if let Some(provider) = Provider::from_name(name).filter(|p| !clients.is_available(*p)) {
            let why = clients.unavailable.iter().find(|(p, _)| *p == provider).map(|(_, w)| w.clone()).unwrap_or_else(|| "no credentials".to_string());
            return Err(Error::invalid(format!("{name} can't be asked for art on this machine: {why} (Config › Provider credentials)")));
        }
    }
    Ok(())
}

/// The gate a run applies to every download: the settings, or the types asked for.
fn ask_of(request: &MediaBackfillRequest) -> media::Ask {
    if request.is_asked() {
        media::Ask::Asked(request.types.clone())
    } else {
        media::Ask::Auto
    }
}

pub fn one_liner(s: &MediaBackfillSummary) -> String {
    if let Some(sync) = &s.via_server {
        return format!("{}: {} from the server, {} from providers, {} not available", sync.source, sync.from_server, sync.from_provider, sync.failed);
    }
    let unavailable: u64 = s.unavailable.values().sum();
    let mut line = format!("{} release(s), {} downloaded, {} entr(ies) already complete", s.releases, s.downloaded, s.already);
    if s.linked > 0 {
        line.push_str(&format!(", {} newly linked to TheGamesDB", s.linked));
    }
    if s.serial_releases > 0 {
        line.push_str(&format!(", {} PS1/PS2 release(s) looked up by serial", s.serial_releases));
    }
    if s.restored > 0 {
        line.push_str(&format!(", {} missing file(s) put back", s.restored));
    }
    if unavailable > 0 {
        line.push_str(&format!(", {unavailable} skipped ({} not configured)", s.unavailable.keys().cloned().collect::<Vec<_>>().join(", ")));
    }
    if s.list_calls > 0 {
        line.push_str(&format!(", {} API call(s)", s.list_calls));
    }
    if !s.errors.is_empty() {
        line.push_str(&format!(", {} error(s)", s.errors.len()));
    }
    if let Some(why) = &s.stopped {
        line.push_str(&format!(" -- stopped: {why}"));
    }
    line
}

/// Which releases a request covers, as a filter on `release r` (one `?` for the platform
/// when the request names one). `selected`: the releases a selection came to, in place of
/// the scope.
fn scope_filter(request: &MediaBackfillRequest, selected: Option<&[u64]>) -> String {
    let mut filter = String::from("r.deleted = FALSE");
    if let Some(ids) = selected {
        // Ids, not text: safe to put in the statement as they are.
        let list = if ids.is_empty() { "0".to_string() } else { ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",") };
        filter.push_str(&format!(" AND r.id IN ({list})"));
    } else if request.scope == "owned" {
        filter.push_str(" AND EXISTS (SELECT 1 FROM local_release_games lr JOIN local_game lg ON lg.id = lr.game_id AND lg.deleted = FALSE WHERE lr.release_id = r.id)");
    }
    if request.platform.is_some() {
        filter.push_str(" AND r.app_platform_id = ?");
    }
    filter
}

/// Art linked to a release in scope (or to its title) that has a URL to fetch it from but
/// no file on disk: put back from a cache tier that still has it, else downloaded again
/// (`media::refetch`). Returns how many files came back.
///
/// The check is one `stat` per linked row, so it costs nothing next to the provider pass
/// that follows -- and without it that pass skips every one of these, because a row counts
/// as held whether or not its file exists.
async fn restore_missing_files(app: &App, request: &MediaBackfillRequest, summary: &mut MediaBackfillSummary, progress: &Progress) -> Result<()> {
    let filter = scope_filter(request, None);
    let sql = format!(
        "SELECT m.id, m.path, m.root_role FROM media m WHERE m.deleted = FALSE AND m.source_url IS NOT NULL AND m.source_url <> '' AND ( \
           EXISTS (SELECT 1 FROM release_media rm JOIN `release` r ON r.id = rm.release_id WHERE rm.media_id = m.id AND {filter}) \
           OR EXISTS (SELECT 1 FROM title_media tm JOIN title_releases tr ON tr.title_id = tm.title_id JOIN `release` r ON r.id = tr.release_id WHERE tm.media_id = m.id AND {filter})) \
         ORDER BY m.id"
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    if let Some(platform) = request.platform {
        query = query.bind(platform).bind(platform);
    }
    let rows = query.fetch_all(&app.pool).await?;
    let roots = crate::database::media_store::Roots::load(&app.pool, &app.paths.media_dir()).await?;
    let mut missing = Vec::new();
    for row in &rows {
        let path = roots.resolve(row.try_get::<Option<String>, _>("root_role")?.as_deref(), &row.try_get::<String, _>("path")?);
        if !path.is_file() {
            missing.push(row.try_get::<u64, _>("id")?);
        }
    }
    let total = missing.len() as u64;
    for (i, id) in missing.into_iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("putting back art whose file is missing: {} of {total} ({} done)", i + 1, summary.restored)).await;
        match super::media::refetch(app, id).await {
            Ok(_) => summary.restored += 1,
            Err(e) => {
                log::warn!("media_backfill: media {id}: {}", e.message);
                if summary.errors.len() < 50 {
                    summary.errors.push(format!("media {id}: {}", e.message));
                }
            }
        }
    }
    Ok(())
}

/// Every `(provider, id)` linked to a release in scope -- through the release itself or its
/// title -- with the releases it covers, in a stable order.
async fn entries(app: &App, request: &MediaBackfillRequest, selected: Option<&[u64]>) -> Result<(u64, Vec<((Provider, String), Vec<MediaTarget>)>)> {
    let filter = scope_filter(request, selected);
    let sql = format!(
        "SELECT r.id AS release_id, l.provider, l.provider_id FROM `release` r \
         JOIN release_provider_links l ON l.release_id = r.id AND l.deleted = FALSE WHERE {filter} \
         UNION \
         SELECT r.id AS release_id, l.provider, l.provider_id FROM `release` r \
         JOIN title_releases tr ON tr.release_id = r.id \
         JOIN title_provider_links l ON l.title_id = tr.title_id AND l.deleted = FALSE WHERE {filter} \
           AND NOT EXISTS (SELECT 1 FROM release_provider_links ro WHERE ro.release_id = r.id AND ro.provider = l.provider AND ro.deleted = FALSE AND NOT {apart}) \
         ORDER BY provider, provider_id, release_id",
        // A release of a title combined from several games, with a game of its own there:
        // its own art, not the title's game's.
        apart = crate::database::providers::cache::APART_SQL
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    if let Some(platform) = request.platform {
        query = query.bind(platform).bind(platform);
    }
    let rows = query.fetch_all(&app.pool).await?;
    let mut grouped: BTreeMap<(String, String), Vec<MediaTarget>> = BTreeMap::new();
    let mut releases = std::collections::HashSet::new();
    for row in &rows {
        let release_id: u64 = row.try_get("release_id")?;
        let provider: String = row.try_get("provider")?;
        let provider_id: String = row.try_get("provider_id")?;
        let Some(p) = Provider::from_name(&provider) else { continue };
        if media::provider_media(p).is_empty() {
            continue;
        }
        // Only the providers asked for; with none named, only those whose art comes on its
        // own -- a manual-only one would be counted here and then do nothing.
        let named = request.providers.iter().any(|n| n == p.name());
        if if request.providers.is_empty() { p.media_manual_only() } else { !named } {
            continue;
        }
        releases.insert(release_id);
        grouped.entry((provider, provider_id)).or_default().push(MediaTarget::Release(release_id));
    }
    let list = grouped.into_iter().filter_map(|((provider, id), targets)| Provider::from_name(&provider).map(|p| ((p, id), targets))).collect();
    Ok((releases.len() as u64, list))
}

pub async fn run(app: &App, request: &MediaBackfillRequest, progress: &Progress) -> Result<MediaBackfillSummary> {
    validate(request)?;
    let narrowed = request.selection.is_some() || !request.providers.is_empty() || request.is_asked();
    let scope = if request.selection.is_some() { "selection".to_string() } else { request.scope.clone() };
    let mut summary = MediaBackfillSummary { scope, ..Default::default() };
    // A client's art comes from its main server, which already knows what this machine lacks.
    if settings::instance_mode(&app.pool).await? == settings::InstanceMode::Client {
        if narrowed {
            return Err(Error::invalid("download art for a selection on the main server; this machine takes its art from there"));
        }
        summary.via_server = Some(super::media_sync::run(app, None, progress).await?);
        return Ok(summary);
    }
    let selected = match &request.selection {
        Some(selection) => {
            let mut warnings = Vec::new();
            let ids = super::selection::release_ids(app, selection, true, &mut warnings).await?;
            summary.errors.extend(warnings);
            Some(ids)
        }
        None => None,
    };
    let ask = ask_of(request);
    // Started again (a restart, a retry): go on after the last entry the earlier run finished
    // -- it checkpoints as it goes -- rather than checking thousands of done entries again.
    let resume = progress.checkpoint().await;
    let after: Option<(String, String)> = resume.as_deref().and_then(|c| c.strip_prefix("entries:")).map(|k| match k.split_once('\t') {
        Some((p, id)) => (p.to_string(), id.to_string()),
        None => (String::new(), String::new()),
    });
    // Where the serial covers go on from: the start on a fresh run, the release after the
    // checkpointed one on a resumed one, nowhere once the run is past them.
    let serial_from: Option<u64> = match resume.as_deref() {
        None => Some(0),
        Some(c) => c.strip_prefix("xlenore:").map(|id| id.parse().unwrap_or(0)),
    };
    // A selection run is about new art for what was picked; putting back the whole
    // collection's missing files is the full run's business.
    if resume.is_none() && selected.is_none() {
        progress.force(0, None, "looking for art whose file is missing").await;
        restore_missing_files(app, request, &mut summary, progress).await?;
        progress.save_checkpoint("xlenore:0").await;
    }
    // Covers by serial before the providers: a free source, and where it ranks above them
    // what it gives is not then fetched from a provider as well.
    let serial_asked = request.providers.iter().any(|p| p == xlenore::NAME);
    if let Some(from) = serial_from.filter(|_| request.providers.is_empty() || serial_asked) {
        serial_covers(app, request, selected.as_deref(), &ask, from, &mut summary, progress).await?;
        progress.save_checkpoint("entries:").await;
    }
    if serial_asked && request.providers.iter().all(|p| p == xlenore::NAME) {
        if summary.serial_releases == 0 {
            summary.stopped = Some("no PS1 or PS2 release in scope has a serial to look its cover up by".to_string());
        }
        return Ok(summary);
    }
    // TheGamesDB asked for: link what is in scope from its free dump first, each release to
    // its own region's entry -- so a set needs no sync of its own, and no allowance is spent
    // linking. A per-platform sync would cost a call per 20 games of the whole platform.
    if ask.is_asked() && request.providers.iter().any(|p| p == Provider::TheGamesDb.name()) {
        progress.force(0, None, "linking to TheGamesDB from its dump").await;
        let ids: Vec<u64> = match &selected {
            Some(ids) => ids.clone(),
            None => {
                let mut q = sqlx::query_scalar::<_, u64>(sqlx::AssertSqlSafe(format!("SELECT r.id FROM `release` r WHERE {}", scope_filter(request, None))));
                if let Some(platform) = request.platform {
                    q = q.bind(platform);
                }
                q.fetch_all(&app.pool).await?
            }
        };
        let regions = crate::database::providers::thegamesdb_region::TgdbRegions::load(&app.clients(), &app.paths.provider_cache_dir()).await?;
        let priority = settings::region_priority(&app.pool).await?;
        let (linked, cached) = crate::database::providers::thegamesdb_region::link_releases(&app.pool, &regions, &priority, &ids).await?;
        if !cached {
            summary.stopped = Some("TheGamesDB's game list isn't downloaded here yet: run a thegamesdb \"full\" sync once (Jobs › Metadata request) -- it is its free dump and uses no allowance".to_string());
            return Ok(summary);
        }
        summary.linked = linked;
    }
    progress.force(0, None, "finding linked releases").await;
    let (releases, entries) = entries(app, request, selected.as_deref()).await?;
    summary.releases = releases;
    summary.entries = entries.len() as u64;
    // Asked for, and nothing to ask: say why, as a failed job, rather than "done, 0".
    if entries.is_empty() && (request.is_asked() || !request.providers.is_empty()) {
        let which = if request.providers.is_empty() { "any provider with art".to_string() } else { request.providers.join(", ") };
        let what = match &selected {
            Some(ids) => format!("none of the {} selected release(s)", ids.len()),
            None => "no release in scope".to_string(),
        };
        let how = if request.providers.iter().any(|p| p == Provider::TheGamesDb.name()) { "no name matched TheGamesDB's list: link them by hand on the release page" } else { "run a sync of it (Jobs › Metadata request) or link them by hand first" };
        summary.stopped = Some(format!("{what} is linked to {which}: {how}"));
        return Ok(summary);
    }
    let total = entries.len() as u64;
    // Entries come in (provider, id) order: the first one past the checkpoint is where to go
    // on, whether or not the checkpointed one is still in the list.
    let start = match &after {
        Some((p, id)) if !p.is_empty() => entries.partition_point(|((provider, provider_id), _)| (provider.name(), provider_id.as_str()) <= (p.as_str(), id.as_str())),
        _ => 0,
    };
    if start > 0 {
        progress.force(start as u64, Some(total), &format!("going on from an earlier run: {start} of {total} provider entries already done")).await;
    }
    let store = RawStore::new(app.paths.provider_cache_dir(), settings::raw_retention(&app.pool).await?);
    let clients = app.clients();
    let media_dir = app.paths.media_dir();
    // TheGamesDB's image lists, for the types asked: fetched for many games per call, ahead
    // of the entries that need them.
    let slot_types: Vec<&str> = request.types.iter().map(|t| t.split_once(':').map_or(t.as_str(), |(t, _)| t)).collect();
    let tgdb_kinds = tgdb_media::tgdb_types(&slot_types);
    let mut saved = std::time::Instant::now();
    // IGDB games whose localized covers were looked up ahead in this run.
    let mut igdb_ahead: std::collections::HashSet<i32> = std::collections::HashSet::new();
    for (i, ((provider, provider_id), targets)) in entries.iter().enumerate().skip(start) {
        // IGDB's localized covers, likewise: 100 games per call, not up to three calls each.
        if *provider == Provider::Igdb && clients.igdb.is_some() {
            let root = store.root();
            let unseen = |id: &i32| !igdb_ahead.contains(id) && igdb_localized::cached(root, *id).is_none();
            if provider_id.parse::<i32>().is_ok_and(|id| unseen(&id)) {
                let ahead: Vec<i32> = entries[i..].iter().filter(|((p, _), _)| *p == Provider::Igdb).filter_map(|((_, id), _)| id.parse::<i32>().ok()).filter(unseen).take(100).collect();
                igdb_ahead.extend(ahead.iter().copied());
                match igdb_localized::prefetch_ids(&clients, &app.pool, root, &ahead).await {
                    Ok(calls) => summary.list_calls += calls,
                    Err(e) => {
                        if summary.errors.len() < 50 {
                            summary.errors.push(format!("igdb localized covers: {e}"));
                        }
                    }
                }
            }
        }
        if *provider == Provider::TheGamesDb && ask.is_asked() && !tgdb_kinds.is_empty() {
            let id = provider_id.parse::<i64>().unwrap_or(0);
            if !tgdb_media::cached(store.root(), id).is_some_and(|c| c.covers(&tgdb_kinds)) && clients.thegamesdb.is_some() {
                let ahead: Vec<i64> = entries[i..].iter().filter(|((p, _), _)| *p == Provider::TheGamesDb).filter_map(|((_, id), _)| id.parse::<i64>().ok()).filter(|id| !tgdb_media::cached(store.root(), *id).is_some_and(|c| c.covers(&tgdb_kinds))).take(tgdb_media::BATCH).collect();
                match tgdb_media::prefetch(&clients, &app.pool, store.root(), &ahead, &tgdb_kinds).await {
                    Ok(f) => {
                        summary.list_calls += f.calls;
                        if f.used_up && !tgdb_media::cached(store.root(), id).is_some_and(|c| c.covers(&tgdb_kinds)) {
                            stop_here(&mut summary, &entries, i, progress).await;
                            break;
                        }
                    }
                    Err(e) => {
                        if summary.errors.len() < 50 {
                            summary.errors.push(format!("thegamesdb images: {e}"));
                        }
                    }
                }
            }
        }
        progress.report(i as u64, Some(total), &format!("{} of {total} provider entries ({releases} release(s)), {} downloaded", i + 1, summary.downloaded)).await;
        // Every few seconds, the entry before this one is done.
        if i > start && saved.elapsed() >= std::time::Duration::from_secs(5) {
            let ((p, id), _) = &entries[i - 1];
            progress.save_checkpoint(&format!("entries:{}\t{id}", p.name())).await;
            saved = std::time::Instant::now();
        }
        match media::attach_for(&app.http, &app.pool, &media_dir, &clients, &store, *provider, provider_id, targets, &ask).await {
            Ok(Attached::Already) => summary.already += 1,
            Ok(Attached::Linked(n)) => summary.downloaded += n as u64,
            Ok(Attached::Unavailable) => *summary.unavailable.entry(provider.name().to_string()).or_default() += 1,
            Ok(Attached::Unsupported) => {}
            Err(e) if e.to_string().contains(tgdb_media::USED_UP) => {
                stop_here(&mut summary, &entries, i, progress).await;
                break;
            }
            Err(e) => {
                log::warn!("media_backfill: {} {provider_id}: {e}", provider.name());
                if summary.errors.len() < 50 {
                    summary.errors.push(format!("{} {provider_id}: {e}", provider.name()));
                }
            }
        }
    }
    Ok(summary)
}

/// xlenore's PS1 / PS2 covers for the releases in scope that have a serial, in release id
/// order from `from` on (a resumed run). Each release asks only for what it still wants, and
/// a serial found without a cover is not asked for again for a month.
async fn serial_covers(app: &App, request: &MediaBackfillRequest, selected: Option<&[u64]>, ask: &media::Ask, from: u64, summary: &mut MediaBackfillSummary, progress: &Progress) -> Result<()> {
    let platforms = xlenore::platform_ids().iter().map(u16::to_string).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT r.id, r.app_platform_id, r.serial FROM `release` r WHERE {} AND r.app_platform_id IN ({platforms}) AND r.serial IS NOT NULL AND r.serial <> '' AND r.id > ? ORDER BY r.id",
        scope_filter(request, selected)
    );
    let mut query = sqlx::query_as::<_, (u64, u16, String)>(sqlx::AssertSqlSafe(sql));
    if let Some(platform) = request.platform {
        query = query.bind(platform);
    }
    let releases = query.bind(from).fetch_all(&app.pool).await?;
    if releases.is_empty() {
        return Ok(());
    }
    let rules = settings::MediaRules::load(&app.pool).await?;
    let media_dir = app.paths.media_dir();
    let mut misses = xlenore::Misses::load(&app.paths.provider_cache_dir());
    let total = releases.len() as u64;
    let mut saved = std::time::Instant::now();
    // Offline, or GitHub refusing: stop after a run of failures instead of one per release.
    let mut failed_in_a_row = 0;
    for (i, (id, platform, serial)) in releases.iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("PS1/PS2 covers by serial: {} of {total}, {} downloaded", i + 1, summary.downloaded)).await;
        if i > 0 && saved.elapsed() >= std::time::Duration::from_secs(5) {
            progress.save_checkpoint(&format!("xlenore:{}", releases[i - 1].0)).await;
            misses.save();
            saved = std::time::Instant::now();
        }
        summary.serial_releases += 1;
        match xlenore::attach_release(&app.http, &app.pool, &media_dir, &rules, ask, &mut misses, *id, *platform, serial).await {
            Ok(outcome) => {
                failed_in_a_row = 0;
                summary.downloaded += outcome.linked as u64;
            }
            Err(e) => {
                log::warn!("media_backfill: xlenore {serial}: {e}");
                if summary.errors.len() < 50 {
                    summary.errors.push(format!("xlenore {serial}: {e}"));
                }
                failed_in_a_row += 1;
                if failed_in_a_row >= 20 {
                    summary.errors.push("xlenore: 20 covers in a row failed; the rest were skipped this time".to_string());
                    break;
                }
            }
        }
    }
    misses.save();
    Ok(())
}

/// The run ends before entry `i`, the allowance used up: the entry before it is the
/// checkpoint, so running it again goes on from `i`.
async fn stop_here(summary: &mut MediaBackfillSummary, entries: &[((Provider, String), Vec<MediaTarget>)], i: usize, progress: &Progress) {
    if i > 0 {
        let ((p, id), _) = &entries[i - 1];
        progress.save_checkpoint(&format!("entries:{}\t{id}", p.name())).await;
    } else {
        progress.save_checkpoint("entries:").await;
    }
    summary.stopped = Some(format!("TheGamesDB's monthly allowance is used up, {} of {} entries left; run it again next month to go on", entries.len() - i, entries.len()));
}
