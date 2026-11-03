//! Hash-first, name-second identification of one scanned thing against the catalog.
//!
//! The four shapes a request can take, and what comes back:
//! - a hash the catalog knows -> [`MatchOutcome::Hash`] (no scoring involved; the title
//!   passed along, if any, is ignored -- hash identity wins);
//! - an unknown hash plus a name -> name candidates, gated and scored ->
//!   [`MatchOutcome::Name`] / [`MatchOutcome::Ambiguous`];
//! - a name only -> same as above;
//! - nothing the catalog knows -> [`MatchOutcome::None`].

use sqlx::mysql::MySqlPool;

use crate::common::models::{HashAlgorithms, Platform};

use super::super::error::DatabaseError;
use super::super::lookup::{self, HashMatch, ReleaseCandidate};
use super::name_parse::{parse_name, ParsedName};
use super::scoring::{gate_and_score, pick_best, CandidateGate, CandidateScore, Selection};

#[derive(Debug, Clone, Default)]
pub struct ResolveRequest {
    pub hashes: Vec<(HashAlgorithms, String)>,
    pub name: Option<String>,
    pub platform: Option<Platform>,
    /// How many name candidates to pull from the catalog before scoring.
    pub candidate_limit: u32,
}

impl ResolveRequest {
    pub fn new() -> Self {
        Self { candidate_limit: 50, ..Default::default() }
    }

    pub fn hash(mut self, algorithm: HashAlgorithms, value: impl Into<String>) -> Self {
        self.hashes.push((algorithm, value.into()));
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn platform(mut self, platform: Platform) -> Self {
        self.platform = Some(platform);
        self
    }
}

#[derive(Debug, Clone)]
pub enum MatchOutcome {
    /// Hash identity: which algorithm hit and every catalog row carrying that hash.
    Hash { algorithm: HashAlgorithms, matches: Vec<HashMatch> },
    /// No hash identity; a unique best name candidate.
    Name { best: ReleaseCandidate, score: CandidateScore, candidates: usize },
    /// No hash identity; several candidates tied at the top.
    Ambiguous { candidates: Vec<ReleaseCandidate> },
    None,
}

impl MatchOutcome {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Hash { .. } => "hash",
            Self::Name { .. } => "name",
            Self::Ambiguous { .. } => "ambiguous",
            Self::None => "none",
        }
    }
}

pub async fn resolve(pool: &MySqlPool, request: &ResolveRequest) -> Result<MatchOutcome, DatabaseError> {
    if !request.hashes.is_empty() {
        if let Some((algorithm, matches)) = lookup::lookup_by_hashes(pool, &request.hashes).await? {
            return Ok(MatchOutcome::Hash { algorithm, matches });
        }
    }
    let Some(name) = request.name.as_deref().filter(|n| !n.trim().is_empty()) else { return Ok(MatchOutcome::None) };
    let candidates = lookup::search_releases_by_name(pool, name, request.platform, request.candidate_limit.max(1)).await?;
    if candidates.is_empty() {
        return Ok(MatchOutcome::None);
    }
    let parsed = parse_name(name);
    let dat_codes: Vec<&str> = parsed.regions.iter().flat_map(|r| r.all_codes()).collect();
    let scored: Vec<(&ReleaseCandidate, CandidateScore)> = candidates
        .iter()
        .filter_map(|cand| {
            let cand_parsed: ParsedName = parse_name(&cand.name);
            let cand_codes: Vec<&str> = cand.region.as_deref().map(|r| r.split(',').map(str::trim).collect()).unwrap_or_default();
            let cand_platforms: Vec<i64> = if cand.platform == Platform::Unknown { Vec::new() } else { vec![cand.platform.igdb_id() as i64] };
            let dat_platform = request.platform.filter(|p| *p != Platform::Unknown).map(|p| p.igdb_id() as i64);
            match gate_and_score(&parsed, Some(&cand_parsed), cand.year, &cand_codes, &["WORLD", "US", "EU"], Some(&cand_platforms), dat_platform, &dat_codes) {
                CandidateGate::Pass(score) => Some((cand, score)),
                CandidateGate::Reject => None,
            }
        })
        .collect();
    let total = scored.len();
    match pick_best(scored.iter().map(|(c, s)| (*c, *s))) {
        Selection::Best(best) => {
            let score = scored.iter().find(|(c, _)| c.release_id == best.release_id).map(|(_, s)| *s).expect("scored");
            Ok(MatchOutcome::Name { best: best.clone(), score, candidates: total })
        }
        Selection::Ambiguous => {
            let top = scored.iter().map(|(_, s)| *s).max().expect("non-empty");
            let tied: Vec<&ReleaseCandidate> = scored.iter().filter(|(_, s)| *s == top).map(|(c, _)| *c).collect();
            // Several sources listing the same release (No-Intro and TOSEC both carrying it,
            // say) tie by construction; when every tied candidate belongs to one title that
            // is a match on the title, not an ambiguity. Only genuinely different titles
            // (or candidates without a title yet) are reported as ambiguous.
            let same_title = tied.first().and_then(|c| c.title_id).filter(|first| tied.iter().all(|c| c.title_id == Some(*first)));
            match same_title {
                Some(_) => Ok(MatchOutcome::Name { best: tied[0].clone(), score: top, candidates: total }),
                None => Ok(MatchOutcome::Ambiguous { candidates: tied.into_iter().cloned().collect() }),
            }
        }
        Selection::None => Ok(MatchOutcome::None),
    }
}
