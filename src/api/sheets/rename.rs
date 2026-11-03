//! Bringing a dump's file names into line with the convention its DAT uses.
//!
//! This is the one operation in romexplorer that **moves the user's own ROM files**, so it is
//! built to be refusable at every step. It computes the whole old-to-new table first and
//! changes nothing; the caller shows that table; only an explicit confirmation applies it. In
//! a job it is off unless it was asked for. Nothing here is implicit in creating a sheet.
//!
//! **Why it belongs next to sheet creation.** A cue names its tracks and a playlist names its
//! discs, so the names *are* the content of the file being written. Writing `Game (Track 1).bin`
//! into a cue when the file on disk is `game_t1.bin` produces a cue that does not work; writing
//! the real name produces one that works but that no DAT will ever match. The user's answer to
//! that is to standardise the names, and this is that.
//!
//! **What "the convention" means.** The default is the DAT's own file names
//! (`release_contents.file_name`) -- not a pattern this app invented, but the exact strings the
//! release is defined by, which is what makes the renamed dump verifiable afterwards. A user
//! who wants something else supplies a pattern instead.
//!
//! Three refusals, all of them before anything moves: a target that already exists, two
//! sources that would land on one target, and a name that is not a legal file name. Each is
//! reported against the row it came from rather than aborting the table, so one bad name does
//! not hide the other twenty.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use sqlx::Row;

use super::super::{App, Error, Result};

/// One file's proposed move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub file_id: u64,
    pub from: PathBuf,
    pub to: PathBuf,
    /// Set when this row cannot be applied; the rest of the table still can.
    pub problem: Option<String>,
}

impl Move {
    pub fn changes_anything(&self) -> bool {
        self.problem.is_none() && self.from != self.to
    }
}

/// The whole proposal, computed without touching anything.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub moves: Vec<Move>,
    /// True when every file is already named as it should be -- the common case, and the one
    /// where no prompt should appear at all.
    pub already_consistent: bool,
}

impl Plan {
    pub fn applicable(&self) -> usize {
        self.moves.iter().filter(|m| m.changes_anything()).count()
    }

    pub fn problems(&self) -> Vec<String> {
        self.moves.iter().filter_map(|m| m.problem.as_ref().map(|p| format!("{}: {p}", m.from.display()))).collect()
    }
}

/// What to rename each file to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scheme {
    /// The names the DAT gives, which is what makes the result matchable. The default.
    Dat,
    /// A user-supplied pattern, with `{name}` for the dump's name, `{track}` / `{disc}` for the
    /// number and `{ext}` for the existing extension.
    Pattern(String),
}

impl Default for Scheme {
    fn default() -> Self {
        Self::Dat
    }
}

/// Fills a pattern for one file.
///
/// Padding is deliberate and matches the cue convention `chd_cd::track_file_name` uses: track
/// numbers are bare below ten tracks and two-digit from ten up, so a rename does not invent a
/// spelling the sheet writer would never produce.
pub fn fill(pattern: &str, name: &str, number: Option<u32>, total: usize, extension: &str) -> String {
    let number_text = number
        .map(|n| if total >= 10 { format!("{n:02}") } else { n.to_string() })
        .unwrap_or_default();
    pattern
        .replace("{name}", name)
        .replace("{track}", &number_text)
        .replace("{disc}", &number_text)
        .replace("{ext}", extension)
}

/// Whether a dump's files already follow one scheme.
///
/// The test is not "do they match the DAT" -- plenty of perfectly tidy collections use their
/// own naming -- but "do they agree with each other": one shared stem, differing only in the
/// track or disc marker. A set that already agrees is left alone, because renaming it would be
/// churn in service of nothing.
pub fn consistent(names: &[String]) -> bool {
    if names.len() < 2 {
        return true;
    }
    let stems: HashSet<String> = names.iter().map(|n| crate::api::scan::disc_set_key(n).0.to_ascii_lowercase()).collect();
    stems.len() == 1
}

/// Computes the proposal for one game's files.
///
/// `files` are `(file_id, path)` in the order the sheet will name them; `dat_names` are the
/// DAT's names for the same files, in the same order, where the release supplies them.
pub fn plan(files: &[(u64, PathBuf)], dat_names: &[String], scheme: &Scheme, dump_name: &str) -> Plan {
    let total = files.len();
    let mut moves = Vec::with_capacity(total);
    let mut targets: HashMap<PathBuf, usize> = HashMap::new();

    for (index, (file_id, from)) in files.iter().enumerate() {
        let extension = from.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
        let wanted = match scheme {
            Scheme::Dat => dat_names.get(index).cloned(),
            Scheme::Pattern(pattern) => Some(fill(pattern, dump_name, Some(index as u32 + 1), total, &extension)),
        };
        let Some(wanted) = wanted.filter(|w| !w.trim().is_empty()) else {
            moves.push(Move { file_id: *file_id, from: from.clone(), to: from.clone(), problem: Some("nothing says what this should be called".to_string()) });
            continue;
        };
        // Sanitised, then checked against what came back: a name the filesystem would mangle
        // is refused rather than silently rewritten, so what the table shows is what happens.
        let safe = crate::api::copy::sanitize_segment(&wanted);
        let problem = (safe != wanted).then(|| format!("{wanted:?} isn't a legal file name (it would become {safe:?})"));
        let to = from.parent().unwrap_or(Path::new(".")).join(&safe);
        *targets.entry(to.clone()).or_default() += 1;
        moves.push(Move { file_id: *file_id, from: from.clone(), to, problem });
    }

    // A collision would silently destroy one of the two files, so both rows are refused.
    for m in moves.iter_mut() {
        if m.problem.is_none() && targets.get(&m.to).copied().unwrap_or(0) > 1 {
            m.problem = Some(format!("two files would both become {}", m.to.display()));
        }
        // Something already sitting at the target that is not this file.
        if m.problem.is_none() && m.from != m.to && m.to.exists() {
            m.problem = Some(format!("{} already exists", m.to.display()));
        }
    }

    let already_consistent = moves.iter().all(|m| m.from == m.to);
    Plan { moves, already_consistent }
}

/// Applies a plan: renames on disk, then moves the rows to match.
///
/// Order matters and is not arbitrary. The file is renamed first and the database second, so a
/// crash between them leaves a row pointing at a path that is gone -- which the scanner already
/// understands (it marks the file missing and re-finds it by content). The reverse order would
/// leave a row pointing at a path that exists and holds something else, which nothing detects.
///
/// Each file is its own step. A failure part-way leaves earlier renames done and is reported;
/// rolling them back would mean a second set of renames that could fail in turn.
pub async fn apply(app: &App, plan: &Plan) -> Result<Applied> {
    let mut out = Applied::default();
    for m in plan.moves.iter().filter(|m| m.changes_anything()) {
        if let Err(e) = std::fs::rename(&m.from, &m.to) {
            out.errors.push(format!("{} -> {}: {e}", m.from.display(), m.to.display()));
            continue;
        }
        let path = m.to.display().to_string();
        let updated = sqlx::query("UPDATE local_files SET path = ?, path_hash = SHA1(?) WHERE id = ?").bind(&path).bind(&path).bind(m.file_id).execute(&app.pool).await;
        if let Err(e) = updated {
            // The bytes moved; say so rather than reporting a clean failure, because the two
            // are now out of step and only the truth helps.
            out.errors.push(format!("renamed {} but couldn't record it: {e}", m.to.display()));
            continue;
        }
        // A rom whose main file this was follows it, or the rom points at nothing.
        let from = m.from.display().to_string();
        sqlx::query("UPDATE local_rom SET main_path = ? WHERE main_path = ?").bind(&path).bind(&from).execute(&app.pool).await?;
        out.renamed += 1;
    }
    Ok(out)
}

#[derive(Debug, Clone, Default)]
pub struct Applied {
    pub renamed: u64,
    pub errors: Vec<String>,
}

/// The DAT's names for a game's files, in the order the sheet names them.
pub async fn dat_names(app: &App, game_id: u64) -> Result<HashMap<u64, String>> {
    let rows = sqlx::query(
        "SELECT f.id AS file_id, rc.file_name FROM local_release_games lrg \
         JOIN release_contents rc ON rc.release_id = lrg.release_id \
         JOIN local_game_roms lgr ON lgr.game_id = lrg.game_id \
         JOIN local_rom_files lrf ON lrf.rom_id = lgr.rom_id \
         JOIN local_files f ON f.id = lrf.file_id AND f.deleted = FALSE AND f.content_id = rc.content_id \
         WHERE lrg.game_id = ?",
    )
    .bind(game_id)
    .fetch_all(&app.pool)
    .await?;
    rows.iter().map(|r| Ok((r.try_get("file_id")?, r.try_get("file_name")?))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(id: u64, path: &str) -> (u64, PathBuf) {
        (id, PathBuf::from(path))
    }

    #[test]
    fn the_dat_names_are_the_default_and_land_beside_the_originals() {
        let files = vec![at(1, "/roms/psx/g_t1.bin"), at(2, "/roms/psx/g_t2.bin")];
        let names = vec!["Game (Track 1).bin".to_string(), "Game (Track 2).bin".to_string()];
        let plan = plan(&files, &names, &Scheme::Dat, "Game");
        assert_eq!(plan.applicable(), 2);
        assert_eq!(plan.moves[0].to, PathBuf::from("/roms/psx/Game (Track 1).bin"));
        assert!(plan.problems().is_empty());
    }

    /// Both rows are refused, not one: silently picking a winner destroys the other file.
    #[test]
    fn two_files_that_would_collide_are_both_refused() {
        let files = vec![at(1, "/roms/a.bin"), at(2, "/roms/b.bin")];
        let names = vec!["Same.bin".to_string(), "Same.bin".to_string()];
        let plan = plan(&files, &names, &Scheme::Dat, "Game");
        assert_eq!(plan.applicable(), 0);
        assert_eq!(plan.problems().len(), 2, "{:?}", plan.problems());
    }

    /// A name the filesystem would mangle is refused, so the table shows what will happen.
    #[test]
    fn an_illegal_name_is_refused_rather_than_quietly_fixed() {
        let files = vec![at(1, "/roms/a.bin")];
        let plan = plan(&files, &["Game: Special?.bin".to_string()], &Scheme::Dat, "Game");
        assert_eq!(plan.applicable(), 0);
        assert!(plan.problems()[0].contains("isn't a legal file name"), "{:?}", plan.problems());
    }

    #[test]
    fn a_pattern_fills_its_placeholders() {
        assert_eq!(fill("{name} (Track {track}).{ext}", "Game", Some(2), 3, "bin"), "Game (Track 2).bin");
        assert_eq!(fill("{name} (Disc {disc}).{ext}", "Game", Some(1), 2, "chd"), "Game (Disc 1).chd");
    }

    /// Ten or more tracks pad, matching what the cue writer itself produces.
    #[test]
    fn track_numbers_pad_exactly_as_the_sheet_writer_pads_them() {
        assert_eq!(fill("{track}", "G", Some(2), 9, "bin"), "2");
        assert_eq!(fill("{track}", "G", Some(2), 12, "bin"), "02");
    }

    /// Tidy-but-not-DAT names are left alone: the test is whether they agree with each other.
    #[test]
    fn a_set_that_already_agrees_needs_no_renaming() {
        assert!(consistent(&["Game (Disc 1).chd".to_string(), "Game (Disc 2).chd".to_string()]));
        assert!(consistent(&["my_game_d1.chd".to_string(), "my_game_d1.chd".to_string()]));
        assert!(!consistent(&["Game (Disc 1).chd".to_string(), "Something Else (Disc 2).chd".to_string()]));
        assert!(consistent(&["only one.chd".to_string()]), "one file always agrees with itself");
    }

    #[test]
    fn a_plan_that_changes_nothing_says_so() {
        let files = vec![at(1, "/roms/Game (Track 1).bin")];
        let plan = plan(&files, &["Game (Track 1).bin".to_string()], &Scheme::Dat, "Game");
        assert!(plan.already_consistent);
        assert_eq!(plan.applicable(), 0);
    }
}
