//! Creating and checking the index files a dump needs: `.cue`, `.gdi`, `.m3u`.

use serde::{Deserialize, Serialize};

/// Which sidecar an operation is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SheetTarget {
    /// The disc sheet a dump's own DAT names -- `.cue` or `.gdi`, whichever this dump uses.
    /// One choice rather than two, because the dump decides: a GD-ROM's tracks cannot be
    /// described by a cue and a CD's cannot be described by a gdi.
    #[default]
    Sheet,
    M3u,
}

impl SheetTarget {
    pub fn name(self) -> &'static str {
        match self {
            Self::Sheet => "sheet",
            Self::M3u => "m3u",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Sheet => "disc sheet (.cue / .gdi)",
            Self::M3u => "playlist (.m3u)",
        }
    }
}

/// How far a sidecar got when it was checked.
///
/// Two answers, kept apart on purpose. `verdict` is whether it *works*; `hash_known`
/// distinguishes "nothing to compare against" from "compared, and it differs". A sheet that
/// plays but is not byte-identical to Redump's is never called broken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetVerdictView {
    /// `verified` | `structure_ok` | `broken`.
    pub verdict: String,
    pub hash_known: bool,
    /// One line per thing a person could go and fix.
    #[serde(default)]
    pub problems: Vec<String>,
}

impl SheetVerdictView {
    /// The badge text.
    pub fn label(&self) -> String {
        match self.verdict.as_str() {
            "verified" => "verified".to_string(),
            "broken" => format!("broken ({})", self.problems.len().max(1)),
            _ if self.hash_known => "structure ok, not the DAT's".to_string(),
            _ => "structure ok".to_string(),
        }
    }

    /// `ok` / `warn` / `bad`, matching the badge classes the rest of the UI uses.
    pub fn tone(&self) -> &'static str {
        match self.verdict.as_str() {
            "verified" => "ok",
            "broken" => "bad",
            _ => "warn",
        }
    }
}

/// One dump, and whether a sidecar can be built for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SheetCandidate {
    pub game_id: u64,
    pub game_name: String,
    pub target: SheetTarget,
    /// The extension that would actually be written (`cue`, `gdi` or `m3u`), once the dump has
    /// said which it uses.
    pub extension: String,
    pub destination: String,
    /// The tracks or discs it would name, in order.
    pub members: Vec<String>,
    /// `None` when it can be built; otherwise why the button is greyed out.
    pub blocked: Option<String>,
    /// Whether the catalogue records this sheet, so a rebuild could be confirmed against it.
    pub checkable: bool,
    /// Set when the dump's files disagree with each other about naming, so the caller can offer
    /// to standardise them. Empty means they already agree and nothing should be offered.
    #[serde(default)]
    pub rename: Vec<RenameRow>,
}

impl SheetCandidate {
    pub fn eligible(&self) -> bool {
        self.blocked.is_none()
    }
}

/// One proposed rename, shown before anything moves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenameRow {
    pub file_id: u64,
    pub from: String,
    pub to: String,
    /// Set when this row will be skipped: a collision, an existing file, an illegal name.
    pub problem: Option<String>,
}

/// What a preview concluded for a selection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SheetPlan {
    pub candidates: Vec<SheetCandidate>,
    pub eligible: u64,
    pub blocked: u64,
    /// Which dialect a rebuild would be written in.
    pub style: String,
}

/// Build one or more sidecars.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SheetCreateRequest {
    /// Games to act on. A rom id is resolved to its game first: a missing cue is a property of
    /// the dump, never of one rom.
    #[serde(default)]
    pub game_ids: Vec<u64>,
    #[serde(default)]
    pub rom_ids: Vec<u64>,
    #[serde(default)]
    pub target: SheetTarget,
    /// Every eligible dump, rather than a named list -- what the Jobs screen enqueues.
    #[serde(default)]
    pub all: bool,
    /// Rename the dump's files to match before writing. Off unless asked: this moves the user's
    /// own ROM files, and nothing else in romexplorer does that.
    #[serde(default)]
    pub standardize: bool,
    /// The naming pattern, when the user supplied one. `None` means the DAT's own names, which
    /// is what makes the renamed dump verifiable afterwards.
    pub pattern: Option<String>,
    /// Required for a run that renames: the caller has seen the old-to-new table. A rename
    /// request without it is refused.
    #[serde(default)]
    pub rename_confirmed: bool,
}

/// Check the sidecars a machine already holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SheetVerifyRequest {
    /// Narrow to one directory, as the integrity sweep does.
    pub directory_id: Option<u64>,
    /// Keep a run short, so it can run often.
    pub limit: Option<u64>,
    /// Only files not checked in this many days.
    pub older_than_days: Option<u64>,
    /// Also fix what can be fixed: a sheet or playlist that works but does not match the DAT
    /// is rewritten to match it (`api::sheets::fix::fix_one`). Off: only check.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fix: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SheetSummary {
    pub created: u64,
    /// Of those, how many reproduce what the catalogue records -- the strongest thing a rebuild
    /// can claim.
    pub matched: u64,
    pub renamed: u64,
    /// Dumps passed over, with the reason each was not eligible.
    pub skipped: Vec<String>,
    pub errors: Vec<String>,
    /// What was written, for the job's own record.
    pub paths: Vec<String>,
    /// The scanned directories the written files are in: a quick scan of each records them
    /// (and links a playlist to its game) right away.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub directories: Vec<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VerifySummary {
    pub checked: u64,
    pub verified: u64,
    pub structure_ok: u64,
    /// With `fix`: rewritten to match the DAT (counted in `verified` too).
    #[serde(default)]
    pub fixed: u64,
    /// With `fix`: sound but not fixable, with why.
    #[serde(default)]
    pub not_fixed: Vec<String>,
    /// Each broken sheet, with its first problem.
    pub broken: Vec<String>,
    pub errors: Vec<String>,
}

/// What a hash repair concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixOutcome {
    /// Rewritten, and it now matches the DAT exactly.
    Fixed,
    /// It already did.
    #[default]
    NothingToFix,
    /// The track files are not named the way the DAT names them, so no rewriting of the sheet
    /// can ever match -- the names in a sheet have to be the names on disk. Renaming the ROMs
    /// is a separate operation with its own confirmation.
    NamesNotCanonical,
    /// Every legal spelling was tried and none reproduces what the DAT records. The dump
    /// probably differs from the DAT's in a way a sheet cannot express.
    NoConventionMatches,
    /// No DAT records this sheet, so there is nothing to aim at.
    NotCheckable,
    /// It has a real fault, which a hash fix would only paper over.
    Broken,
}

impl FixOutcome {
    /// Whether the sheet changed on disk.
    pub fn changed(self) -> bool {
        matches!(self, Self::Fixed)
    }

    /// The heading over the message.
    pub fn label(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::NothingToFix => "already correct",
            Self::NamesNotCanonical => "can't fix: the file names don't conform",
            Self::NoConventionMatches => "can't fix: no spelling of this sheet matches",
            Self::NotCheckable => "nothing to check against",
            Self::Broken => "needs more than a hash fix",
        }
    }

    /// `ok` / `warn` / `bad` for the message that follows.
    pub fn tone(self) -> &'static str {
        match self {
            Self::Fixed | Self::NothingToFix => "ok",
            Self::Broken => "bad",
            _ => "warn",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FixResult {
    pub outcome: FixOutcome,
    /// Written for someone reading it in a dialog, and always saying whether the disc works.
    pub message: String,
    /// The specific files or faults behind the outcome.
    #[serde(default)]
    pub problems: Vec<String>,
}

#[cfg(test)]
mod fix_tests {
    use super::*;

    #[test]
    fn only_a_real_repair_counts_as_a_change() {
        assert!(FixOutcome::Fixed.changed());
        for outcome in [FixOutcome::NothingToFix, FixOutcome::NamesNotCanonical, FixOutcome::NoConventionMatches, FixOutcome::NotCheckable, FixOutcome::Broken] {
            assert!(!outcome.changed(), "{outcome:?}");
        }
    }

    /// A refusal is not a failure. Only a sheet with a real fault is red; the rest work.
    #[test]
    fn only_a_genuinely_faulty_sheet_reads_as_bad() {
        assert_eq!(FixOutcome::Broken.tone(), "bad");
        assert_eq!(FixOutcome::NamesNotCanonical.tone(), "warn");
        assert_eq!(FixOutcome::NoConventionMatches.tone(), "warn");
        assert_eq!(FixOutcome::Fixed.tone(), "ok");
    }

    #[test]
    fn every_outcome_says_what_it_means() {
        for outcome in [FixOutcome::Fixed, FixOutcome::NothingToFix, FixOutcome::NamesNotCanonical, FixOutcome::NoConventionMatches, FixOutcome::NotCheckable, FixOutcome::Broken] {
            assert!(!outcome.label().is_empty(), "{outcome:?}");
        }
    }
}
