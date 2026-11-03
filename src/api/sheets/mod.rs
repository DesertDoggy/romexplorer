//! Creating and checking the index files a dump needs: `.cue`, `.gdi`, `.m3u`.
//!
//! Three sidecars, one shape of problem. The tracks (or the discs) are all there, and the file
//! that says how they fit together is not -- so an emulator sees loose bins instead of a disc,
//! or four separate games instead of one four-disc game. `rom::sheet_build` knows how to write
//! those files; this module works out *which* dumps need one, does it against the collection,
//! and records what it concluded.
//!
//! **The unit is the dump, not the rom, and the difference is not pedantry.** With its cue, a
//! cue/bin set is one `local_rom`: the cue is `main_path` and the tracks are its
//! `local_rom_files` (`rom::scan` consumes them). *Without* it, nothing groups the tracks, so
//! each `.bin` is scanned as a rom of its own and they land in one `local_game` only because
//! they share a release. A missing cue is therefore never a property of one rom -- it is a
//! property of the game -- and the rom screen's button resolves upward before it does
//! anything. Creating the cue collapses those roms back into one on the next scan, which is
//! the repair, not a side effect of it.
//!
//! **What "missing only the cue" means.** `api::catalog::coverage` already answers which of a
//! release's files a game holds, and already treats sheets specially (`SHEET_EXTS`). This asks
//! that question with the sheet line put back in: every non-sheet line present, the sheet line
//! absent. Anything else -- a missing track, a sheet already there -- is not eligible, and the
//! button is disabled rather than hidden so the reason can be shown.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sqlx::Row;

use crate::rom::detect::sheet::{CueSheetOptions, LineEnding, SheetKind};
use crate::rom::sheet_build::{self, Expected, SheetVerdict};

use super::{App, Error, Result};

pub mod fix;
pub mod rename;
pub mod run;

/// Which sidecar a dump is short of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetTarget {
    Cue,
    Gdi,
    M3u,
}

impl SheetTarget {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Cue => "cue",
            Self::Gdi => "gdi",
            Self::M3u => "m3u",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "cue" => Some(Self::Cue),
            "gdi" => Some(Self::Gdi),
            "m3u" => Some(Self::M3u),
            _ => None,
        }
    }

    pub fn sheet_kind(self) -> Option<SheetKind> {
        match self {
            Self::Cue => Some(SheetKind::Cue),
            Self::Gdi => Some(SheetKind::Gdi),
            Self::M3u => None,
        }
    }
}

/// Why a dump cannot have a sidecar built for it right now.
///
/// Each of these is a disabled button with a reason attached, never a silent absence: "why is
/// this greyed out" is the question the screen has to answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ineligible {
    /// It already has one. The common case, and the reason the button is greyed rather than
    /// missing.
    AlreadyThere { path: String },
    /// Files other than the sidecar are missing, so there is nothing whole to describe. This
    /// is the case the user named: a dump short of a `.bin` is not short of a `.cue`.
    Incomplete { missing: Vec<String> },
    /// One disc. A playlist over a single disc groups nothing.
    SingleDisc,
    /// The discs are in more than one folder and no folder holds them all.
    SplitFolders { stray: String },
    /// No DAT release, so there is no statement of what the dump should contain and no way to
    /// tell "missing its cue" from "a pile of loose tracks".
    Unmatched,
    /// A platform whose dumps do not take this sidecar at all.
    WrongPlatform { platform: String },
}

impl Ineligible {
    pub fn reason(&self) -> String {
        match self {
            Self::AlreadyThere { path } => format!("it already has one: {path}"),
            Self::Incomplete { missing } => {
                format!("{} file(s) are missing besides the sheet, so there's no whole dump to describe: {}", missing.len(), missing.join(", "))
            }
            Self::SingleDisc => "a playlist over one disc groups nothing".to_string(),
            Self::SplitFolders { stray } => format!("{stray} is in another folder and no one folder holds every disc, so one playlist can't name them all"),
            Self::Unmatched => "not matched to a DAT release, so nothing says what this dump should contain".to_string(),
            Self::WrongPlatform { platform } => format!("{platform} dumps don't use this"),
        }
    }
}

/// One dump and what could be done for it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub game_id: u64,
    pub game_name: String,
    pub target: SheetTarget,
    /// Absolute path the sidecar would be written to.
    pub destination: PathBuf,
    /// The track files (cue/gdi) or disc files (m3u) it would name, in order.
    pub members: Vec<PathBuf>,
    /// What the catalogue records about the sidecar itself, when a DAT lists one.
    pub expected: Expected,
    /// `None` means it can be built.
    pub blocked: Option<Ineligible>,
}

impl Candidate {
    pub fn eligible(&self) -> bool {
        self.blocked.is_none()
    }
}

/// The disc-sheet extensions, matching `api::catalog::SHEET_EXTS` -- the same set, because the
/// same judgement ("a container carries no copy of this") governs both.
const SHEET_EXTS: [&str; 4] = ["cue", "gdi", "ccd", "m3u"];

fn ext_of(name: &str) -> String {
    Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase()
}

/// One release line: which DAT source listed it, its name, its content, and whether the game
/// holds it.
struct Line {
    source_id: Option<u8>,
    file_name: String,
    content_id: u64,
    present: bool,
}

/// The release lines of `game`, with whether each is held -- the same `present` test
/// `catalog::decorate_games` uses, asked for one game.
async fn release_lines(app: &App, game_id: u64) -> Result<Vec<Line>> {
    let rows = sqlx::query(
        "SELECT rc.source_id, rc.file_name, rc.content_id, \
            EXISTS (SELECT 1 FROM local_game_roms lgr JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id \
                JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE \
                LEFT JOIN content_contents cc ON cc.parent_content_id = f.content_id \
                WHERE lgr.game_id = lrg.game_id AND (f.content_id = rc.content_id OR cc.child_content_id = rc.content_id)) AS present \
         FROM local_release_games lrg JOIN release_contents rc ON rc.release_id = lrg.release_id \
         WHERE lrg.game_id = ?",
    )
    .bind(game_id)
    .fetch_all(&app.pool)
    .await?;
    rows.iter()
        .map(|r| {
            Ok(Line {
                source_id: r.try_get("source_id")?,
                file_name: r.try_get("file_name")?,
                content_id: r.try_get("content_id")?,
                present: r.try_get::<i64, _>("present")? != 0,
            })
        })
        .collect()
}

/// What the catalogue records for one content: the size and hashes a rebuild can be checked
/// against.
pub(crate) async fn expected_for(app: &App, content_id: u64) -> Result<Expected> {
    let size: Option<u64> = sqlx::query_scalar("SELECT size FROM content WHERE id = ?").bind(content_id).fetch_optional(&app.pool).await?.flatten();
    let hashes: Vec<(String, String)> = sqlx::query_as("SELECT algorithm, value_hex FROM hashes WHERE content_id = ?").bind(content_id).fetch_all(&app.pool).await?;
    Ok(Expected { size, hashes })
}

/// The live files of a game's roms, in disc then path order.
async fn game_files(app: &App, game_id: u64) -> Result<Vec<(u64, PathBuf, Option<u16>)>> {
    let rows = sqlx::query(
        "SELECT f.id, f.path, lr.disc_number FROM local_game_roms lgr \
         JOIN local_rom lr ON lr.id = lgr.rom_id AND lr.deleted = FALSE \
         JOIN local_rom_files lrf ON lrf.rom_id = lr.id \
         JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE \
         WHERE lgr.game_id = ? ORDER BY lr.disc_number, f.path",
    )
    .bind(game_id)
    .fetch_all(&app.pool)
    .await?;
    rows.iter().map(|r| Ok((r.try_get("id")?, PathBuf::from(r.try_get::<String, _>("path")?), r.try_get("disc_number")?))).collect()
}

/// Works out what `game_id` is short of, for `target`.
///
/// The judgement is per DAT source, not pooled: Redump's cue/bin dump and mame-redump's CHD
/// describe the same release and a line missing from one is not missing from the other. So it
/// picks the source whose dump is closest to whole, exactly as `catalog::best_coverage` does,
/// and reports against that one.
pub async fn examine(app: &App, game_id: u64, target: SheetTarget) -> Result<Candidate> {
    let mut candidate = examine_dump(app, game_id, target).await?;
    // A file already where this one would go -- made by an earlier run, or by hand, and not
    // scanned since, so nothing here knows of it -- is the dump having one, not an error. A
    // second run used to report every playlist the first had written as a failure.
    if candidate.blocked.is_none() && candidate.destination.exists() {
        candidate.blocked = Some(Ineligible::AlreadyThere { path: format!("{} (not scanned yet)", candidate.destination.display()) });
    }
    Ok(candidate)
}

async fn examine_dump(app: &App, game_id: u64, target: SheetTarget) -> Result<Candidate> {
    let row = sqlx::query("SELECT name, playlist_file_id FROM local_game WHERE id = ? AND deleted = FALSE")
        .bind(game_id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("no game {game_id}")))?;
    let game_name: String = row.try_get("name")?;
    let playlist_file_id: Option<u64> = row.try_get("playlist_file_id")?;
    let files = game_files(app, game_id).await?;

    let mut candidate = Candidate {
        game_id,
        game_name: game_name.clone(),
        target,
        destination: PathBuf::new(),
        members: Vec::new(),
        expected: Expected::default(),
        blocked: None,
    };

    if target == SheetTarget::M3u {
        return examine_m3u(app, candidate, &files, playlist_file_id).await;
    }

    let lines = release_lines(app, game_id).await?;
    if lines.is_empty() {
        candidate.blocked = Some(Ineligible::Unmatched);
        return Ok(candidate);
    }
    // Per source, because two DATs describe two different dumps of one release.
    let mut by_source: HashMap<Option<u8>, Vec<&Line>> = HashMap::new();
    for line in &lines {
        by_source.entry(line.source_id).or_default().push(line);
    }
    // The dump closest to whole wins, and among equals the one that actually names the sheet
    // we were asked for -- otherwise a source with no cue line at all looks "complete" and
    // hides the one that is genuinely short of its cue.
    let wanted = target.extension();
    let best = by_source
        .values()
        .max_by_key(|lines| {
            let payload: Vec<&&Line> = lines.iter().filter(|l| !SHEET_EXTS.contains(&ext_of(&l.file_name).as_str())).collect();
            let missing = payload.iter().filter(|l| !l.present).count();
            let names_it = lines.iter().any(|l| ext_of(&l.file_name) == wanted);
            (payload.len() > 0, std::cmp::Reverse(missing), names_it, payload.len())
        })
        .ok_or_else(|| Error::internal("no release lines"))?;

    let sheet_line = best.iter().find(|l| ext_of(&l.file_name) == wanted);
    let Some(sheet_line) = sheet_line else {
        candidate.blocked = Some(Ineligible::WrongPlatform { platform: format!("this dump of {game_name}") });
        return Ok(candidate);
    };
    if sheet_line.present {
        let held = files.iter().find(|(_, path, _)| ext_of(&path.to_string_lossy()) == wanted);
        candidate.blocked = Some(Ineligible::AlreadyThere { path: held.map(|(_, p, _)| p.display().to_string()).unwrap_or_else(|| sheet_line.file_name.clone()) });
        return Ok(candidate);
    }
    // Everything that is not a sheet has to be here; that is the "almost complete" test.
    let missing: Vec<String> = best.iter().filter(|l| !l.present && !SHEET_EXTS.contains(&ext_of(&l.file_name).as_str())).map(|l| l.file_name.clone()).collect();
    if !missing.is_empty() {
        candidate.blocked = Some(Ineligible::Incomplete { missing });
        return Ok(candidate);
    }

    // The tracks, in the order the DAT lists them -- which is track order, and is the one
    // ordering a rebuilt sheet must not get wrong.
    let mut payload: Vec<&&Line> = best.iter().filter(|l| !SHEET_EXTS.contains(&ext_of(&l.file_name).as_str())).collect();
    payload.sort_by(|a, b| natural_track_order(&a.file_name).cmp(&natural_track_order(&b.file_name)).then(a.file_name.cmp(&b.file_name)));
    let by_content: HashMap<u64, &PathBuf> = HashMap::new();
    let _ = by_content;
    let held_by_content = held_contents(app, game_id).await?;
    let mut members = Vec::with_capacity(payload.len());
    for line in &payload {
        match held_by_content.get(&line.content_id) {
            Some(path) => members.push(path.clone()),
            None => {
                candidate.blocked = Some(Ineligible::Incomplete { missing: vec![line.file_name.clone()] });
                return Ok(candidate);
            }
        }
    }
    let directory = members.first().and_then(|p| p.parent()).map(Path::to_path_buf).ok_or_else(|| Error::invalid("the tracks have no folder"))?;
    // The sheet is named as the DAT names it, so the dump matches its release exactly.
    candidate.destination = directory.join(&sheet_line.file_name);
    candidate.members = members;
    candidate.expected = expected_for(app, sheet_line.content_id).await?;
    Ok(candidate)
}

/// Where each content a game holds actually lives.
async fn held_contents(app: &App, game_id: u64) -> Result<HashMap<u64, PathBuf>> {
    let rows = sqlx::query(
        "SELECT f.content_id, f.path FROM local_game_roms lgr \
         JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id \
         JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE \
         WHERE lgr.game_id = ?",
    )
    .bind(game_id)
    .fetch_all(&app.pool)
    .await?;
    rows.iter().map(|r| Ok((r.try_get("content_id")?, PathBuf::from(r.try_get::<String, _>("path")?)))).collect()
}

/// `(Track 2)` sorts before `(Track 10)`: the DAT's own order is track order, and a plain
/// string sort would put track 10 second and silently reorder the disc.
fn natural_track_order(name: &str) -> (u32, String) {
    lazy_static::lazy_static! {
        static ref RE_TRACK: regex::Regex = regex::Regex::new(r"(?i)\(track\s*(\d+)\)").unwrap();
    }
    let number = RE_TRACK.captures(name).and_then(|c| c.get(1)).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
    (number, name.to_ascii_lowercase())
}

/// A playlist's eligibility, which asks nothing of a DAT: a playlist is not part of any dump,
/// it is a convenience over several of them, so the test is simply "several complete discs and
/// nothing grouping them yet".
async fn examine_m3u(app: &App, mut candidate: Candidate, files: &[(u64, PathBuf, Option<u16>)], playlist_file_id: Option<u64>) -> Result<Candidate> {
    if let Some(id) = playlist_file_id {
        let path: Option<String> = sqlx::query_scalar("SELECT path FROM local_files WHERE id = ? AND deleted = FALSE").bind(id).fetch_optional(&app.pool).await?;
        if let Some(path) = path {
            candidate.blocked = Some(Ineligible::AlreadyThere { path });
            return Ok(candidate);
        }
    }
    // One entry per rom: a playlist lists discs, and a disc's tracks are not discs.
    let all = main_paths(app, candidate.game_id).await?;
    // Discs of one game normally sit together; when they do not, a relative playlist cannot
    // name them all and an absolute one breaks on the first copy. Copies of a disc kept in
    // another folder as well (the same disk image in two machines' sets) are left out.
    let (directory, discs) = match playlist_folder(&all) {
        Ok(found) => found,
        Err(stray) => {
            candidate.blocked = Some(Ineligible::SplitFolders { stray: stray.display().to_string() });
            return Ok(candidate);
        }
    };
    if discs.len() < 2 {
        candidate.blocked = Some(Ineligible::SingleDisc);
        return Ok(candidate);
    }
    let missing: Vec<String> = discs.iter().filter(|path| !path.exists()).map(|p| p.display().to_string()).collect();
    let _ = files;
    if !missing.is_empty() {
        candidate.blocked = Some(Ineligible::Incomplete { missing });
        return Ok(candidate);
    }
    // The set's own name: the discs' shared name with "(Disc N)" taken off, which is what
    // `api::scan::disc_set_key` already extracts to group them.
    let stem = discs
        .first()
        .and_then(|p| p.file_stem())
        .map(|s| crate::api::scan::disc_set_key(&s.to_string_lossy()).0)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| candidate.game_name.clone());
    candidate.destination = directory.join(format!("{}.m3u", crate::api::copy::sanitize_segment(&stem)));
    candidate.members = discs;
    Ok(candidate)
}

/// The main file of each of a game's roms, in disc order, with its disc number when known --
/// one line of the playlist each.
async fn main_paths(app: &App, game_id: u64) -> Result<Vec<(PathBuf, Option<u16>)>> {
    let rows: Vec<(String, Option<u16>)> = sqlx::query_as(
        "SELECT lr.main_path, lr.disc_number FROM local_game_roms lgr JOIN local_rom lr ON lr.id = lgr.rom_id AND lr.deleted = FALSE \
         WHERE lgr.game_id = ? ORDER BY lr.disc_number IS NULL, lr.disc_number, lr.main_path",
    )
    .bind(game_id)
    .fetch_all(&app.pool)
    .await?;
    Ok(rows.into_iter().map(|(p, n)| (PathBuf::from(p), n)).collect())
}

/// The folder a playlist goes in, and the discs it lists (in order): the folder holding them
/// all, or -- when copies of some disc sit elsewhere too -- the one folder that holds a disc
/// of every number the game has (the most discs first), one disc per number. `Err` names a
/// disc in another folder when no folder holds them all.
pub fn playlist_folder(discs: &[(PathBuf, Option<u16>)]) -> std::result::Result<(PathBuf, Vec<PathBuf>), PathBuf> {
    let folder = |p: &PathBuf| p.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut folders: Vec<PathBuf> = Vec::new();
    for (p, _) in discs {
        let f = folder(p);
        if !folders.contains(&f) {
            folders.push(f);
        }
    }
    if folders.len() <= 1 {
        return Ok((folders.into_iter().next().unwrap_or_default(), discs.iter().map(|(p, _)| p.clone()).collect()));
    }
    let stray = || discs.iter().map(|(p, _)| p.clone()).find(|p| folder(p) != folders[0]).unwrap_or_default();
    // Without a number on every disc, nothing says which discs are copies of which.
    if discs.iter().any(|(_, n)| n.is_none()) {
        return Err(stray());
    }
    let needed: std::collections::BTreeSet<u16> = discs.iter().filter_map(|(_, n)| *n).collect();
    let best = folders
        .iter()
        .filter(|f| needed.iter().all(|n| discs.iter().any(|(p, d)| folder(p) == **f && *d == Some(*n))))
        .max_by_key(|f| (discs.iter().filter(|(p, _)| folder(p) == **f).count(), std::cmp::Reverse(folders.iter().position(|x| x == *f))));
    let Some(best) = best else { return Err(stray()) };
    let mut taken = std::collections::BTreeSet::new();
    let chosen: Vec<PathBuf> = discs.iter().filter(|(p, n)| folder(p) == *best && taken.insert(*n)).map(|(p, _)| p.clone()).collect();
    Ok((best.clone(), chosen))
}

/// The game a rom belongs to -- the unit every one of these operations really acts on.
pub async fn game_of_rom(app: &App, rom_id: u64) -> Result<u64> {
    sqlx::query_scalar("SELECT lgr.game_id FROM local_game_roms lgr JOIN local_game lg ON lg.id = lgr.game_id AND lg.deleted = FALSE WHERE lgr.rom_id = ? ORDER BY lg.id LIMIT 1")
        .bind(rom_id)
        .fetch_optional(&app.pool)
        .await?
        .ok_or_else(|| Error::not_found(format!("rom {rom_id} isn't in a game, so there's no dump to build a sheet for")))
}

/// Builds the sidecar `candidate` describes and writes it.
///
/// Never overwrites: the eligibility check already refuses a dump that has one, and this is the
/// backstop for the gap between checking and writing.
pub async fn build(app: &App, candidate: &Candidate) -> Result<Built> {
    if let Some(blocked) = &candidate.blocked {
        return Err(Error::invalid(blocked.reason()));
    }
    if candidate.destination.exists() {
        return Err(Error::conflict(format!("{} already exists", candidate.destination.display())));
    }
    let style = style_setting(app).await?;
    let options = CueSheetOptions::for_style(style);
    let stem = candidate.destination.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();

    let (bytes, matched, note) = match candidate.target {
        SheetTarget::M3u => {
            let names: Vec<String> = candidate.members.iter().map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()).collect();
            (sheet_build::build_m3u(&names, options.line_ending), false, "one line per disc".to_string())
        }
        SheetTarget::Cue | SheetTarget::Gdi => {
            let tracks: Vec<sheet_build::TrackFile> = candidate
                .members
                .iter()
                .map(|p| sheet_build::TrackFile::at(p).map_err(|e| Error::invalid(format!("{}: {e}", p.display()))))
                .collect::<Result<Vec<_>>>()?;
            let rebuilt = match candidate.target {
                SheetTarget::Gdi => sheet_build::rebuild_gdi(&stem, &tracks, &candidate.expected, &options),
                _ => sheet_build::rebuild_cue(&stem, &tracks, &candidate.expected, &options),
            }
            .map_err(Error::invalid)?;
            (rebuilt.bytes, rebuilt.matched, rebuilt.convention.label().to_string())
        }
    };

    let temporary = candidate.destination.with_extension(format!("{}.part", candidate.target.extension()));
    std::fs::write(&temporary, &bytes).map_err(|e| Error::internal(format!("can't write {}: {e}", temporary.display())))?;
    std::fs::rename(&temporary, &candidate.destination).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        Error::internal(format!("can't put {} in place: {e}", candidate.destination.display()))
    })?;
    Ok(Built { path: candidate.destination.clone(), bytes: bytes.len() as u64, matched, note })
}

#[derive(Debug, Clone)]
pub struct Built {
    pub path: PathBuf,
    pub bytes: u64,
    /// Whether it reproduces what the catalogue records for this sheet.
    pub matched: bool,
    /// How it was built, for the summary: which gap convention, or "one line per disc".
    pub note: String,
}

/// Checks one sidecar already on disk and records the verdict against its file row.
pub async fn check_file(app: &App, file_id: u64, path: &Path, content_id: u64) -> Result<SheetVerdict> {
    let expected = expected_for(app, content_id).await?;
    let target = SheetTarget::from_extension(&ext_of(&path.to_string_lossy()));
    let owned = path.to_path_buf();
    let verdict = tokio::task::spawn_blocking(move || match target {
        Some(SheetTarget::M3u) => sheet_build::check_m3u(&owned, &expected),
        _ => sheet_build::check_sheet(&owned, &expected),
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?;
    record_verdict(app, file_id, &verdict).await?;
    Ok(verdict)
}

/// Stores a verdict so the file's row can show it without re-reading the disk.
pub async fn record_verdict(app: &App, file_id: u64, verdict: &SheetVerdict) -> Result<()> {
    let (hash_known, detail) = match verdict {
        SheetVerdict::StructureOk { hash_known } => (*hash_known, None),
        SheetVerdict::Broken { problems } => (false, Some(problems.join("\n"))),
        SheetVerdict::Verified => (true, None),
    };
    sqlx::query(
        "INSERT INTO local_sheet_check (file_id, verdict, hash_known, detail) VALUES (?, ?, ?, ?) \
         ON DUPLICATE KEY UPDATE verdict = VALUES(verdict), hash_known = VALUES(hash_known), detail = VALUES(detail), checked_at = NOW()",
    )
    .bind(file_id)
    .bind(verdict.name())
    .bind(hash_known)
    .bind(detail)
    .execute(&app.pool)
    .await?;
    Ok(())
}

/// Which cue style this installation writes (`sheet_style`), defaulting to Redump's -- the
/// convention every DAT this app reads is written against.
pub async fn style_setting(app: &App) -> Result<crate::common::models::CueSheetStyle> {
    let stored: Option<String> = crate::database::settings::get(&app.pool, crate::database::settings::KEY_SHEET_STYLE).await?;
    Ok(match stored.as_deref() {
        Some("redump_catalog") => crate::common::models::CueSheetStyle::RedumpCatalog,
        Some("chdman") => crate::common::models::CueSheetStyle::Chdman,
        Some("mame") => crate::common::models::CueSheetStyle::Mame,
        _ => crate::common::models::CueSheetStyle::Redump,
    })
}

/// The line ending a generated playlist uses -- the same style setting, so a collection's
/// generated files are consistent with each other.
pub async fn line_ending(app: &App) -> Result<LineEnding> {
    Ok(CueSheetOptions::for_style(style_setting(app).await?).line_ending)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_playlist_takes_the_folder_with_every_disc() {
        let d = |p: &str, n: Option<u16>| (PathBuf::from(p), n);
        let p = |s: &str| PathBuf::from(s);
        // All in one folder: as they are.
        let one = [d("/a/Hoge (Disk 1).dsk", Some(1)), d("/a/Hoge (Disk 2).dsk", Some(2))];
        assert_eq!(playlist_folder(&one), Ok((p("/a"), vec![p("/a/Hoge (Disk 1).dsk"), p("/a/Hoge (Disk 2).dsk")])));
        // A copy of disc 2 in another machine's set: left out.
        let copy = [d("/a/Hoge (Disk 1).dsk", Some(1)), d("/a/Hoge (Disk 2).dsk", Some(2)), d("/b/Hoge (Disk 2 of 3).dsk", Some(2))];
        assert_eq!(playlist_folder(&copy), Ok((p("/a"), vec![p("/a/Hoge (Disk 1).dsk"), p("/a/Hoge (Disk 2).dsk")])));
        // Really split: no folder has every disc.
        let split = [d("/a/Hoge (Disk 1).dsk", Some(1)), d("/b/Hoge (Disk 2).dsk", Some(2))];
        assert_eq!(playlist_folder(&split), Err(p("/b/Hoge (Disk 2).dsk")));
        // No numbers: nothing says which are copies.
        let unnumbered = [d("/a/Hoge A.dsk", None), d("/b/Hoge B.dsk", None)];
        assert!(playlist_folder(&unnumbered).is_err());
    }

    /// Track 10 must not sort between 1 and 2, or the rebuilt sheet reorders the disc.
    #[test]
    fn tracks_sort_by_number_not_by_text() {
        let mut names = vec!["G (Track 10).bin".to_string(), "G (Track 2).bin".to_string(), "G (Track 1).bin".to_string()];
        names.sort_by_key(|n| natural_track_order(n));
        assert_eq!(names, vec!["G (Track 1).bin".to_string(), "G (Track 2).bin".to_string(), "G (Track 10).bin".to_string()]);
    }

    /// A name with no track marker keeps its place rather than jumping to the front.
    #[test]
    fn a_name_without_a_track_number_still_orders() {
        assert_eq!(natural_track_order("Game.bin").0, 0);
        assert_eq!(natural_track_order("Game (Track 3).bin").0, 3);
        assert_eq!(natural_track_order("Game (track 04).cue").0, 4);
    }

    #[test]
    fn every_reason_says_something_a_person_can_act_on() {
        for blocked in [
            Ineligible::AlreadyThere { path: "/x/g.cue".into() },
            Ineligible::Incomplete { missing: vec!["g (Track 2).bin".into()] },
            Ineligible::SingleDisc,
            Ineligible::SplitFolders { stray: "/x/b/g (Disc 2).cue".into() },
            Ineligible::Unmatched,
            Ineligible::WrongPlatform { platform: "SNES".into() },
        ] {
            assert!(!blocked.reason().is_empty());
        }
    }

    #[test]
    fn targets_round_trip_their_extensions() {
        for target in [SheetTarget::Cue, SheetTarget::Gdi, SheetTarget::M3u] {
            assert_eq!(SheetTarget::from_extension(target.extension()), Some(target));
            assert_eq!(SheetTarget::from_extension(&target.extension().to_uppercase()), Some(target));
        }
        assert_eq!(SheetTarget::from_extension("bin"), None);
    }
}
