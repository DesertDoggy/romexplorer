//! Re-checking one sidecar on demand, and repairing one whose only fault is its hash.
//!
//! **What "fix the hash" can and cannot do.** A cue's bytes are its track names and its gap
//! notation, and nothing else. So a sheet that works but does not match the DAT differs in one
//! of exactly two ways, and only one of them is repairable here:
//!
//! - *The gap notation.* Recoverable, because there are only a few legal spellings and the DAT's
//!   hash says which one is right -- the same search [`rom::sheet_build::rebuild_cue`] does when
//!   it builds a sheet from nothing.
//! - *The track names.* Not recoverable by rewriting the sheet, because the names in the sheet
//!   have to be the names on disk. A cue naming `Game (Track 1).bin` when the file is called
//!   `game_t1.bin` is a broken cue, however well it hashes. Fixing that means renaming the ROM
//!   files, which is a different operation with a confirmation of its own
//!   (`api::sheets::rename`), and this refuses rather than quietly reaching for it.
//!
//! So the gate is: are the track files named as the DAT names them? If not, say so plainly --
//! and say in the same breath that the sheet still works, because it does, and someone told
//! that their disc "doesn't match the database" will otherwise go looking for a problem that
//! is not there.

use std::path::{Path, PathBuf};

use romexplorer_types::sheets::{FixOutcome, FixResult, SheetVerdictView};
use sqlx::Row;

use crate::rom::detect::sheet::{CueSheetOptions, Sheet, SheetKind};
use crate::rom::sheet_build::{self, Expected, SheetVerdict};

use super::super::{App, Error, Result};
use super::SheetTarget;

/// One sidecar file, resolved far enough to check or repair.
struct Sidecar {
    file_id: u64,
    path: PathBuf,
    content_id: u64,
    target: SheetTarget,
}

async fn load(app: &App, file_id: u64) -> Result<Sidecar> {
    let row = sqlx::query("SELECT id, path, content_id FROM local_files WHERE id = ? AND deleted = FALSE")
        .bind(file_id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no file {file_id}")))?;
    let path: String = row.try_get("path")?;
    let target = Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(SheetTarget::from_extension)
        .ok_or_else(|| Error::invalid(format!("{path} isn't a cue, gdi or m3u")))?;
    Ok(Sidecar { file_id: row.try_get("id")?, path: PathBuf::from(path), content_id: row.try_get("content_id")?, target })
}

/// Re-reads one sidecar and records what it is worth now.
pub async fn verify_one(app: &App, file_id: u64) -> Result<SheetVerdictView> {
    let sidecar = load(app, file_id).await?;
    let verdict = super::check_file(app, sidecar.file_id, &sidecar.path, sidecar.content_id).await?;
    Ok(view_of(&verdict))
}

pub fn view_of(verdict: &SheetVerdict) -> SheetVerdictView {
    match verdict {
        SheetVerdict::Verified => SheetVerdictView { verdict: "verified".into(), hash_known: true, problems: Vec::new() },
        SheetVerdict::StructureOk { hash_known } => SheetVerdictView { verdict: "structure_ok".into(), hash_known: *hash_known, problems: Vec::new() },
        SheetVerdict::Broken { problems } => SheetVerdictView { verdict: "broken".into(), hash_known: false, problems: problems.clone() },
    }
}

/// The sentence every refusal ends with.
///
/// Not decoration. "Doesn't match the database" reads as a fault, and someone who believes
/// their disc is faulty will go and re-dump it. The structure check is the one that decides
/// whether it plays, and it passed.
const STILL_WORKS: &str = "This isn't a fault: the structure is right, so the disc works and plays exactly as it should. Matching the DAT byte for byte only matters if you want the dump to verify against it.";

/// Tries to make one sidecar match what the catalogue records for it.
pub async fn fix_one(app: &App, file_id: u64) -> Result<FixResult> {
    let sidecar = load(app, file_id).await?;
    let expected = super::expected_for(app, sidecar.content_id).await?;

    // Nothing to compare against means nothing to aim at: a rebuild would be a guess replacing
    // a working file, which is strictly worse than leaving it alone.
    if expected.hashes.is_empty() {
        return Ok(FixResult {
            outcome: FixOutcome::NotCheckable,
            message: format!("No DAT records this {}, so there's nothing to match it against. {STILL_WORKS}", sidecar.target.extension()),
            ..Default::default()
        });
    }

    let current = tokio::task::spawn_blocking({
        let path = sidecar.path.clone();
        let expected = expected.clone();
        let target = sidecar.target;
        move || match target {
            SheetTarget::M3u => sheet_build::check_m3u(&path, &expected),
            _ => sheet_build::check_sheet(&path, &expected),
        }
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?;

    match &current {
        SheetVerdict::Verified => {
            return Ok(FixResult { outcome: FixOutcome::NothingToFix, message: "This already matches the DAT exactly.".to_string(), ..Default::default() })
        }
        // A broken sheet is a different problem with a different answer: something it names is
        // not there, or its numbers do not add up. Rewriting it would paper over that.
        SheetVerdict::Broken { problems } => {
            return Ok(FixResult {
                outcome: FixOutcome::Broken,
                message: format!("This needs more than a hash fix -- {}. Sort that out first, or delete it and create a new one.", problems.join("; ")),
                problems: problems.clone(),
                ..Default::default()
            })
        }
        SheetVerdict::StructureOk { .. } => {}
    }

    // The files the sheet actually names, beside it.
    let directory = sidecar.path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let members = members_of(&sidecar, &directory)?;
    if members.is_empty() {
        return Ok(FixResult { outcome: FixOutcome::Broken, message: "It names no files.".to_string(), ..Default::default() });
    }

    // The gate. A sheet's bytes are its names plus its notation; only the notation is ours to
    // rewrite.
    let offenders = non_canonical(app, &members).await?;
    if !offenders.is_empty() {
        return Ok(FixResult {
            outcome: FixOutcome::NamesNotCanonical,
            message: format!(
                "Can't fix the hash: {} file(s) aren't named the way the DAT names them, and a sheet has to name the files as they actually are -- so no rewriting of this sheet can ever match. \
                 Rename them to the DAT's names first (the create button offers that, with the old-to-new table), then try again. {STILL_WORKS}",
                offenders.len()
            ),
            problems: offenders,
            ..Default::default()
        });
    }

    // Names are right, so the notation is the only thing left that can differ -- and there are
    // only a few legal spellings of it.
    let stem = sidecar.path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let options = CueSheetOptions::for_style(super::style_setting(app).await?);
    let rebuilt = match sidecar.target {
        SheetTarget::M3u => {
            let names: Vec<String> = members.iter().filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string())).collect();
            sheet_build::rebuild_m3u(&stem, &names, &expected, options.line_ending)
        }
        SheetTarget::Gdi => {
            let tracks = track_files(&members)?;
            sheet_build::rebuild_gdi(&stem, &tracks, &expected, &options).map_err(Error::invalid)?
        }
        SheetTarget::Cue => {
            let tracks = track_files(&members)?;
            sheet_build::rebuild_cue(&stem, &tracks, &expected, &options).map_err(Error::invalid)?
        }
    };

    if !rebuilt.matched {
        return Ok(FixResult {
            outcome: FixOutcome::NoConventionMatches,
            message: format!(
                "Tried every way of writing this {} and none of them reproduces what the DAT records. The dump may differ from the DAT's in a way a sheet can't express -- a different pregap, a track of another length. {STILL_WORKS}",
                sidecar.target.extension()
            ),
            ..Default::default()
        });
    }

    // Write through a temporary: the file being replaced is a working one, and a half-written
    // cue is worse than a cue that merely does not match.
    let temporary = sidecar.path.with_extension(format!("{}.part", sidecar.target.extension()));
    std::fs::write(&temporary, &rebuilt.bytes).map_err(|e| Error::internal(format!("can't write {}: {e}", temporary.display())))?;
    std::fs::rename(&temporary, &sidecar.path).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        Error::internal(format!("can't replace {}: {e}", sidecar.path.display()))
    })?;
    super::record_verdict(app, sidecar.file_id, &SheetVerdict::Verified).await?;

    Ok(FixResult {
        outcome: FixOutcome::Fixed,
        message: format!("Rewritten as {} -- it now matches the DAT exactly.", rebuilt.convention.label()),
        ..Default::default()
    })
}

/// The files a sidecar names, resolved beside it.
fn members_of(sidecar: &Sidecar, directory: &Path) -> Result<Vec<PathBuf>> {
    match sidecar.target {
        SheetTarget::M3u => {
            let text = std::fs::read_to_string(&sidecar.path).map_err(|e| Error::internal(format!("{}: {e}", sidecar.path.display())))?;
            Ok(text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(|l| directory.join(l)).collect())
        }
        _ => {
            let kind = if sidecar.target == SheetTarget::Gdi { SheetKind::Gdi } else { SheetKind::Cue };
            let text = std::fs::read_to_string(&sidecar.path).map_err(|e| Error::internal(format!("{}: {e}", sidecar.path.display())))?;
            let sheet = Sheet::parse_text(kind, &text).map_err(Error::invalid)?;
            Ok(sheet.files.iter().map(|n| directory.join(n)).collect())
        }
    }
}

fn track_files(members: &[PathBuf]) -> Result<Vec<sheet_build::TrackFile>> {
    members.iter().map(|p| sheet_build::TrackFile::at(p).map_err(|e| Error::invalid(format!("{}: {e}", p.display())))).collect()
}

/// Which of `members` are not named the way the DAT names them.
///
/// Matched by content, not by position: the DAT's name for a file is a property of its bytes,
/// so a set in the wrong order is still canonically named. A file the catalogue has no name for
/// is not an offender -- nothing says it is wrong, and refusing on "we don't know" would block
/// every dump a DAT happens not to list every part of.
async fn non_canonical(app: &App, members: &[PathBuf]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for path in members {
        let text = path.display().to_string();
        let row = sqlx::query("SELECT content_id FROM local_files WHERE path_hash = SHA1(?) AND deleted = FALSE LIMIT 1").bind(&text).fetch_optional(&app.pool).await?;
        let Some(row) = row else { continue };
        let content_id: u64 = row.try_get("content_id")?;
        let names: Vec<String> = sqlx::query_scalar("SELECT DISTINCT file_name FROM release_contents WHERE content_id = ?").bind(content_id).fetch_all(&app.pool).await?;
        if names.is_empty() {
            continue;
        }
        let actual = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        // Any release's spelling will do: a file listed by two DATs under two names is
        // canonical under either.
        if !names.iter().any(|n| n.eq_ignore_ascii_case(&actual)) {
            out.push(format!("{actual} (the DAT calls it {})", names.join(" or ")));
        }
    }
    Ok(out)
}

/// Whether a `fix` is worth offering at all, for a verdict already on record -- so the button
/// can be absent rather than present-and-always-refusing.
pub fn offerable(view: &SheetVerdictView) -> bool {
    view.verdict == "structure_ok" && view.hash_known
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fix_is_only_offered_where_it_could_help() {
        assert!(offerable(&SheetVerdictView { verdict: "structure_ok".into(), hash_known: true, problems: vec![] }));
        // Nothing to aim at.
        assert!(!offerable(&SheetVerdictView { verdict: "structure_ok".into(), hash_known: false, problems: vec![] }));
        // Already right.
        assert!(!offerable(&SheetVerdictView { verdict: "verified".into(), hash_known: true, problems: vec![] }));
        // A different problem with a different answer.
        assert!(!offerable(&SheetVerdictView { verdict: "broken".into(), hash_known: false, problems: vec!["x".into()] }));
    }

    /// Someone told their disc "doesn't match the database" will go and re-dump it, so every
    /// refusal has to say that it works.
    #[test]
    fn every_refusal_says_the_disc_still_works() {
        assert!(STILL_WORKS.contains("works"));
        assert!(STILL_WORKS.contains("plays"));
    }
}
