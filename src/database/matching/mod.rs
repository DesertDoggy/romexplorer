//! Name-matching building blocks, ported from playmatch's `service/src/matching/`
//! (RetroRealm/playmatch, MIT -- see `app_root_ex/lib/LICENSES/playmatch.LICENSE`):
//! [`name_parse`] splits a DAT/file name into a base title and tags, [`util`] normalizes
//! titles for comparison/indexing, and [`scoring`] gates and ranks candidates. Ported as
//! source (not vendored+patched like `playmatch_deredis`'s provider clients) because these
//! three files are pure functions with no database coupling, and this project extends them
//! (2-letter TOSEC-style region/language codes, serials, `(Alt)`) rather than tracking
//! upstream line-for-line. Upstream's `content_anchor.rs`/`clone.rs`/`manual.rs`/
//! `suggestions.rs` are sea-orm orchestration against playmatch's own schema and are not
//! ported; see docs/plan/database.md ("A fourth track").
//!
//! Priority, per that plan: on an exact hash match nothing here is *matching* at all --
//! `name_parse` only backfills structured fields the DAT didn't carry. Scoring only runs
//! when there is no hash identity (name-search providers, unmatched scanned files); see
//! [`resolve`].

pub mod name_parse;
pub mod resolve;
pub mod scoring;
pub mod util;

pub use name_parse::{parse_name, subset_tag, with_subset_tag, Lang, ParsedName, RegionTag, Variant};
pub use resolve::{resolve, MatchOutcome, ResolveRequest};
pub use scoring::{gate_and_score, pick_best, CandidateGate, CandidateScore, Selection};
pub use util::{clean_name, names_close, names_related, normalize_title};
