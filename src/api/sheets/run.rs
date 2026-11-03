//! The jobs and the preview: resolving a selection, writing the sidecars, checking them.

use std::collections::BTreeSet;
use std::path::PathBuf;

use romexplorer_types::sheets::{RenameRow, SheetCandidate, SheetCreateRequest, SheetPlan, SheetSummary, SheetTarget as WireTarget, SheetVerifyRequest, VerifySummary};
use sqlx::Row;

use crate::database::jobs::Progress;

use super::super::{App, Error, Result};
use super::{rename, Candidate, Ineligible, SheetTarget};

/// Which concrete sidecar a dump wants, when the caller only said "the disc sheet".
///
/// The dump decides, not the user: a GD-ROM's tracks cannot be described by a cue and a CD's
/// cannot be described by a gdi. The DAT says which, by naming one, so this looks rather than
/// asks -- and a dump whose release names neither is simply not a candidate.
async fn resolve(app: &App, game_id: u64, wanted: WireTarget) -> Result<SheetTarget> {
    if wanted == WireTarget::M3u {
        return Ok(SheetTarget::M3u);
    }
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT rc.file_name FROM local_release_games lrg JOIN release_contents rc ON rc.release_id = lrg.release_id WHERE lrg.game_id = ?",
    )
    .bind(game_id)
    .fetch_all(&app.pool)
    .await?;
    let has = |ext: &str| names.iter().any(|n| std::path::Path::new(n).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case(ext)));
    // gdi first: a GD-ROM release lists both on occasion, and the gdi is the one that
    // describes the disc it actually is.
    Ok(if has("gdi") { SheetTarget::Gdi } else { SheetTarget::Cue })
}

/// The games a selection names, with rom ids resolved up to their games.
async fn selected(app: &App, req: &SheetCreateRequest) -> Result<Vec<u64>> {
    let mut ids: BTreeSet<u64> = req.game_ids.iter().copied().collect();
    for rom in &req.rom_ids {
        ids.insert(super::game_of_rom(app, *rom).await?);
    }
    if req.all {
        ids.extend(sweep(app, req.target).await?);
    }
    Ok(ids.into_iter().collect())
}

/// Every game that might want one, as a cheap first pass.
///
/// Deliberately coarse: it narrows tens of thousands of games to the few dozen worth looking
/// at properly, and `examine` makes the real judgement on each. A SQL query that tried to
/// decide eligibility outright would have to reimplement `catalog::coverage`'s per-source rule
/// in SQL, and would then be a second answer to a question that already has one.
async fn sweep(app: &App, target: WireTarget) -> Result<Vec<u64>> {
    let sql = match target {
        // A release that lists a disc sheet whose content nothing here holds.
        WireTarget::Sheet => {
            "SELECT DISTINCT lrg.game_id FROM local_release_games lrg \
             JOIN release_contents rc ON rc.release_id = lrg.release_id \
             JOIN local_game lg ON lg.id = lrg.game_id AND lg.deleted = FALSE \
             WHERE (rc.file_name LIKE '%.cue' OR rc.file_name LIKE '%.gdi') \
               AND NOT EXISTS (SELECT 1 FROM local_files f WHERE f.content_id = rc.content_id AND f.deleted = FALSE)"
        }
        // More than one disc and nothing grouping them.
        WireTarget::M3u => {
            "SELECT lgr.game_id FROM local_game_roms lgr \
             JOIN local_game lg ON lg.id = lgr.game_id AND lg.deleted = FALSE AND lg.playlist_file_id IS NULL \
             JOIN local_rom lr ON lr.id = lgr.rom_id AND lr.deleted = FALSE \
             GROUP BY lgr.game_id HAVING COUNT(DISTINCT lgr.rom_id) > 1"
        }
    };
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).fetch_all(&app.pool).await?)
}

/// What a run would do, with nothing done.
pub async fn preview(app: &App, req: &SheetCreateRequest) -> Result<SheetPlan> {
    let games = selected(app, req).await?;
    let style = super::style_setting(app).await?;
    let mut plan = SheetPlan { style: style.to_string(), ..Default::default() };
    for game_id in games {
        let target = resolve(app, game_id, req.target).await?;
        let candidate = super::examine(app, game_id, target).await?;
        let view = view_of(app, &candidate, req).await?;
        if view.eligible() {
            plan.eligible += 1;
        } else {
            plan.blocked += 1;
        }
        plan.candidates.push(view);
    }
    Ok(plan)
}

/// A candidate as the wire shows it, including the rename table when the dump's names disagree.
async fn view_of(app: &App, candidate: &Candidate, req: &SheetCreateRequest) -> Result<SheetCandidate> {
    let names: Vec<String> = candidate.members.iter().filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string())).collect();
    // Only offered when the files actually disagree with each other; a tidy set that simply
    // is not the DAT's spelling is left alone.
    let rename = if candidate.eligible() && (req.standardize || !rename::consistent(&names)) {
        rename_rows(app, candidate, req).await?
    } else {
        Vec::new()
    };
    Ok(SheetCandidate {
        game_id: candidate.game_id,
        game_name: candidate.game_name.clone(),
        target: if candidate.target == SheetTarget::M3u { WireTarget::M3u } else { WireTarget::Sheet },
        extension: candidate.target.extension().to_string(),
        destination: candidate.destination.display().to_string(),
        members: names,
        blocked: candidate.blocked.as_ref().map(Ineligible::reason),
        checkable: !candidate.expected.hashes.is_empty(),
        rename,
    })
}

async fn rename_rows(app: &App, candidate: &Candidate, req: &SheetCreateRequest) -> Result<Vec<RenameRow>> {
    let plan = rename_plan(app, candidate, req).await?;
    Ok(plan.moves.iter().map(|m| RenameRow { file_id: m.file_id, from: m.from.display().to_string(), to: m.to.display().to_string(), problem: m.problem.clone() }).collect())
}

async fn rename_plan(app: &App, candidate: &Candidate, req: &SheetCreateRequest) -> Result<rename::Plan> {
    let ids = file_ids_for(app, &candidate.members).await?;
    let dat = rename::dat_names(app, candidate.game_id).await?;
    let files: Vec<(u64, PathBuf)> = ids.clone();
    let dat_names: Vec<String> = files.iter().map(|(id, _)| dat.get(id).cloned().unwrap_or_default()).collect();
    let scheme = match req.pattern.as_ref().filter(|p| !p.trim().is_empty()) {
        Some(pattern) => rename::Scheme::Pattern(pattern.clone()),
        None => rename::Scheme::Dat,
    };
    let stem = candidate.destination.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| candidate.game_name.clone());
    Ok(rename::plan(&files, &dat_names, &scheme, &stem))
}

/// The file rows behind a candidate's member paths, in the same order.
async fn file_ids_for(app: &App, paths: &[PathBuf]) -> Result<Vec<(u64, PathBuf)>> {
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let text = path.display().to_string();
        let id: Option<u64> = sqlx::query_scalar("SELECT id FROM local_files WHERE path_hash = SHA1(?) AND deleted = FALSE LIMIT 1").bind(&text).fetch_optional(&app.pool).await?;
        if let Some(id) = id {
            out.push((id, path.clone()));
        }
    }
    Ok(out)
}

/// Builds the sidecars a request asks for.
pub async fn run_create(app: &App, req: &SheetCreateRequest, progress: &Progress) -> Result<SheetSummary> {
    let games = selected(app, req).await?;
    let total = games.len() as u64;
    let mut summary = SheetSummary::default();
    for (i, game_id) in games.iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("dump {} of {total}", i + 1)).await;
        let target = resolve(app, *game_id, req.target).await?;
        let candidate = match super::examine(app, *game_id, target).await {
            Ok(c) => c,
            Err(e) => {
                summary.errors.push(format!("game {game_id}: {}", e.message));
                continue;
            }
        };
        // Not an error: a job over a whole collection is expected to pass over most of it, and
        // a dump that already has its sheet is the commonest reason of all.
        if let Some(blocked) = &candidate.blocked {
            summary.skipped.push(format!("{}: {}", candidate.game_name, blocked.reason()));
            continue;
        }

        // Renaming first, because the sheet has to name the files as they will be, not as they
        // were. Only ever when asked, and only when the caller has seen the table.
        let mut candidate = candidate;
        if req.standardize {
            match rename_plan(app, &candidate, req).await {
                Ok(plan) if plan.applicable() > 0 => {
                    let applied = rename::apply(app, &plan).await?;
                    summary.renamed += applied.renamed;
                    summary.errors.extend(applied.errors);
                    // The members moved, so the candidate has to be recomputed against where
                    // they are now -- otherwise the sheet names the old paths.
                    candidate = super::examine(app, *game_id, target).await?;
                    if candidate.blocked.is_some() {
                        summary.errors.push(format!("{}: renamed, but the dump no longer resolves", candidate.game_name));
                        continue;
                    }
                }
                Ok(plan) => summary.errors.extend(plan.problems()),
                Err(e) => summary.errors.push(format!("{}: {}", candidate.game_name, e.message)),
            }
        }

        match super::build(app, &candidate).await {
            Ok(built) => {
                summary.created += 1;
                if built.matched {
                    summary.matched += 1;
                }
                summary.paths.push(built.path.display().to_string());
                if let Some(first) = candidate.members.first() {
                    let dir: Option<u64> = sqlx::query_scalar("SELECT directory_id FROM local_files WHERE path_hash = SHA1(?) AND deleted = FALSE LIMIT 1").bind(first.display().to_string()).fetch_optional(&app.pool).await?;
                    if let Some(dir) = dir.filter(|d| !summary.directories.contains(d)) {
                        summary.directories.push(dir);
                    }
                }
            }
            Err(e) => summary.errors.push(format!("{}: {}", candidate.game_name, e.message)),
        }
    }
    progress.force(total, Some(total), &create_one_liner(&summary)).await;
    Ok(summary)
}

/// Re-reads the sidecars this machine holds and records what each is worth.
///
/// `m3u` picks which of the two jobs this is. They are separate because they check different
/// things of different files: a sheet is parsed and measured against its tracks, a playlist is
/// walked line by line. Sharing a runner would mean one job type with a mode nobody sets by
/// hand; sharing the sweep means neither re-implements "least recently checked first".
pub async fn run_verify(app: &App, req: &SheetVerifyRequest, progress: &Progress, m3u: bool) -> Result<VerifySummary> {
    let extensions: &[&str] = if m3u { &["m3u"] } else { &["cue", "gdi"] };
    let like = extensions.iter().map(|e| format!("f.path LIKE '%.{e}'")).collect::<Vec<_>>().join(" OR ");
    let mut sql = format!(
        "SELECT f.id, f.path, f.content_id FROM local_files f \
         LEFT JOIN local_sheet_check c ON c.file_id = f.id \
         WHERE f.deleted = FALSE AND ({like})"
    );
    if req.directory_id.is_some() {
        sql.push_str(" AND f.directory_id = ?");
    }
    if req.older_than_days.is_some() {
        sql.push_str(" AND (c.checked_at IS NULL OR c.checked_at < DATE_SUB(NOW(), INTERVAL ? DAY))");
    }
    // Never checked first, then longest ago: short runs repeated over time sweep everything,
    // the same access pattern the integrity sweep uses.
    sql.push_str(" ORDER BY c.checked_at IS NOT NULL, c.checked_at, f.id LIMIT ?");
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
    if let Some(id) = req.directory_id {
        query = query.bind(id);
    }
    if let Some(days) = req.older_than_days {
        query = query.bind(days);
    }
    let rows = query.bind(req.limit.unwrap_or(500).clamp(1, 100_000)).fetch_all(&app.pool).await?;

    let total = rows.len() as u64;
    let mut summary = VerifySummary::default();
    for (i, row) in rows.iter().enumerate() {
        let (id, path, content_id): (u64, String, u64) = (row.try_get("id")?, row.try_get("path")?, row.try_get("content_id")?);
        progress.report(i as u64, Some(total), &format!("{} of {total}: {path}", i + 1)).await;
        match super::check_file(app, id, std::path::Path::new(&path), content_id).await {
            Ok(crate::rom::sheet_build::SheetVerdict::Verified) => {
                summary.checked += 1;
                summary.verified += 1;
            }
            Ok(crate::rom::sheet_build::SheetVerdict::StructureOk { hash_known }) => {
                summary.checked += 1;
                // Asked to fix, and there is a DAT hash to aim at: rewrite it to match.
                if req.fix && hash_known {
                    use romexplorer_types::sheets::FixOutcome;
                    match super::fix::fix_one(app, id).await {
                        Ok(r) if r.outcome == FixOutcome::Fixed => {
                            summary.fixed += 1;
                            summary.verified += 1;
                            continue;
                        }
                        Ok(r) => {
                            let why = match r.outcome {
                                FixOutcome::NamesNotCanonical => "its files aren't named the way the DAT names them (rename them first)".to_string(),
                                FixOutcome::NoConventionMatches => "no way of writing it reproduces the DAT's".to_string(),
                                _ => r.message.split(". ").next().unwrap_or_default().to_string(),
                            };
                            summary.not_fixed.push(format!("{path}: {why}"));
                        }
                        Err(e) => summary.errors.push(format!("{path}: fix: {}", e.message)),
                    }
                }
                summary.structure_ok += 1;
            }
            Ok(crate::rom::sheet_build::SheetVerdict::Broken { problems }) => {
                summary.checked += 1;
                summary.broken.push(format!("{path}: {}", problems.first().cloned().unwrap_or_default()));
            }
            Err(e) => summary.errors.push(format!("{path}: {}", e.message)),
        }
    }
    progress.force(total, Some(total), &verify_one_liner(&summary)).await;
    Ok(summary)
}

/// What `jobs::validate` refuses before anything runs.
pub async fn validate_create(app: &App, req: &SheetCreateRequest) -> Result<()> {
    if req.game_ids.is_empty() && req.rom_ids.is_empty() && !req.all {
        return Err(Error::invalid("nothing selected"));
    }
    // Renaming moves the user's own ROM files. The confirmation is not a formality: it is the
    // caller saying it has shown the old-to-new table and been told to go ahead.
    if req.standardize && !req.rename_confirmed {
        return Err(Error::invalid("renaming files needs an explicit confirmation -- show the preview's rename table first"));
    }
    if let Some(pattern) = req.pattern.as_ref().filter(|p| !p.trim().is_empty()) {
        if !pattern.contains("{name}") && !pattern.contains("{track}") && !pattern.contains("{disc}") {
            return Err(Error::invalid("a naming pattern needs at least one of {name}, {track} or {disc}, or every file would get the same name"));
        }
        let filled = rename::fill(pattern, "Example", Some(1), 1, "bin");
        if crate::api::copy::sanitize_segment(&filled) != filled {
            return Err(Error::invalid(format!("that pattern produces {filled:?}, which isn't a legal file name")));
        }
    }
    let _ = app;
    Ok(())
}

pub fn validate_verify(req: &SheetVerifyRequest) -> Result<()> {
    if req.limit == Some(0) {
        return Err(Error::invalid("a limit of 0 would check nothing"));
    }
    Ok(())
}

pub fn create_one_liner(s: &SheetSummary) -> String {
    let mut parts = vec![format!("{} created", s.created)];
    if s.matched > 0 {
        parts.push(format!("{} matching the DAT", s.matched));
    }
    if s.renamed > 0 {
        parts.push(format!("{} file(s) renamed", s.renamed));
    }
    if !s.skipped.is_empty() {
        parts.push(format!("{} skipped", s.skipped.len()));
    }
    if !s.errors.is_empty() {
        parts.push(format!("{} error(s)", s.errors.len()));
    }
    parts.join(", ")
}

pub fn verify_one_liner(s: &VerifySummary) -> String {
    let mut parts = vec![format!("{} checked", s.checked)];
    if s.verified > 0 {
        parts.push(format!("{} verified", s.verified));
    }
    if s.fixed > 0 {
        parts.push(format!("{} fixed to match the DAT", s.fixed));
    }
    if s.structure_ok > 0 {
        parts.push(format!("{} structurally ok", s.structure_ok));
    }
    if !s.not_fixed.is_empty() {
        parts.push(format!("{} couldn't be fixed", s.not_fixed.len()));
    }
    if !s.broken.is_empty() {
        parts.push(format!("{} BROKEN", s.broken.len()));
    }
    if !s.errors.is_empty() {
        parts.push(format!("{} error(s)", s.errors.len()));
    }
    parts.join(", ")
}
