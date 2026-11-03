//! Candidate gate-then-rank. Ported from playmatch `matching/scoring.rs` (MIT) minus its
//! metrics hook (`record_pick_best` -> prometheus) which this project doesn't have.
//!
//! Only ever runs on the no-hash-identity path (see `super::resolve`): hard-rejects
//! candidates whose platform list excludes the DAT platform, whose year is off by 2+, or
//! that don't carry the same hack/pirate/unlicensed marker, then ranks survivors by year,
//! then region, then variant agreement. Ties at the top are `Ambiguous`, never a guess.

use super::name_parse::{ParsedName, Variant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum YearMatch {
    Unknown,
    OffByOne,
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RegionMatch {
    Unknown,
    Other,
    Default,
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VariantMatch {
    Mismatch,
    Compatible,
    BothNone,
    Same,
}

/// `Ord` ranks year first, then region, then variant agreement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CandidateScore {
    pub year_match: YearMatch,
    pub region_match: RegionMatch,
    pub variant_match: VariantMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateGate {
    Reject,
    Pass(CandidateScore),
}

/// Combined gate + scorer. `candidate_platforms`/`dat_platform_id` are opaque ids (this
/// project passes `platforms.id`); `*_region_codes` are compared case-insensitively.
#[allow(clippy::too_many_arguments)]
pub fn gate_and_score(
    parsed_dat: &ParsedName,
    parsed_cand: Option<&ParsedName>,
    candidate_year: Option<u16>,
    candidate_region_codes: &[&str],
    provider_default_regions: &[&str],
    candidate_platforms: Option<&[i64]>,
    dat_platform_id: Option<i64>,
    dat_region_codes: &[&str],
) -> CandidateGate {
    if let (Some(platforms), Some(dat_pid)) = (candidate_platforms, dat_platform_id) {
        if !platforms.is_empty() && !platforms.contains(&dat_pid) {
            return CandidateGate::Reject;
        }
    }

    if matches!(parsed_dat.variant, Some(Variant::Hack) | Some(Variant::Pirate) | Some(Variant::Unlicensed)) {
        let cand_variant = parsed_cand.and_then(|p| p.variant);
        if cand_variant != parsed_dat.variant {
            return CandidateGate::Reject;
        }
    }

    let year_match = match (parsed_dat.year, candidate_year) {
        (Some(dat_year), Some(cand_year)) => {
            let delta = dat_year.abs_diff(cand_year);
            if delta >= 2 {
                return CandidateGate::Reject;
            } else if delta == 0 {
                YearMatch::Exact
            } else {
                YearMatch::OffByOne
            }
        }
        _ => YearMatch::Unknown,
    };

    let region_match = score_region(candidate_region_codes, dat_region_codes, provider_default_regions);
    let variant_match = score_variant(parsed_dat.variant, parsed_cand.and_then(|p| p.variant));

    CandidateGate::Pass(CandidateScore { year_match, region_match, variant_match })
}

fn score_region(candidate_codes: &[&str], dat_codes: &[&str], provider_default_codes: &[&str]) -> RegionMatch {
    if candidate_codes.is_empty() && dat_codes.is_empty() {
        return RegionMatch::Unknown;
    }
    if !dat_codes.is_empty() && candidate_codes.iter().any(|c| dat_codes.iter().any(|d| d.eq_ignore_ascii_case(c))) {
        return RegionMatch::Exact;
    }
    if candidate_codes.iter().any(|c| provider_default_codes.iter().any(|d| d.eq_ignore_ascii_case(c))) {
        return RegionMatch::Default;
    }
    if !candidate_codes.is_empty() {
        return RegionMatch::Other;
    }
    RegionMatch::Unknown
}

fn score_variant(dat: Option<Variant>, cand: Option<Variant>) -> VariantMatch {
    match (dat, cand) {
        (None, None) => VariantMatch::BothNone,
        (Some(d), Some(c)) if d == c => VariantMatch::Same,
        (Some(Variant::Proto), None) | (Some(Variant::Beta), None) | (Some(Variant::Demo), None) | (Some(Variant::Sample), None) => VariantMatch::Compatible,
        _ => VariantMatch::Mismatch,
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Selection<'a, T> {
    Best(&'a T),
    Ambiguous,
    None,
}

impl<T> Selection<'_, T> {
    pub fn label(&self) -> &'static str {
        match self {
            Selection::Best(_) => "hit",
            Selection::Ambiguous => "ambiguous",
            Selection::None => "miss",
        }
    }
}

/// Returns the unique max-scored candidate. Ties at the top produce `Ambiguous`; empty
/// input produces `None`.
pub fn pick_best<'a, T>(candidates: impl IntoIterator<Item = (&'a T, CandidateScore)>) -> Selection<'a, T> {
    let mut best: Option<(&T, CandidateScore)> = None;
    let mut tied = false;
    for (cand, score) in candidates {
        match best {
            None => {
                best = Some((cand, score));
                tied = false;
            }
            Some((_, b_score)) if score > b_score => {
                best = Some((cand, score));
                tied = false;
            }
            Some((_, b_score)) if score == b_score => {
                tied = true;
            }
            _ => {}
        }
    }
    match best {
        None => Selection::None,
        Some(_) if tied => Selection::Ambiguous,
        Some((c, _)) => Selection::Best(c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::matching::name_parse::RegionTag;

    fn dat(year: Option<u16>) -> ParsedName {
        ParsedName { year, ..ParsedName::default() }
    }

    #[test]
    fn year_gate_and_rank() {
        assert!(matches!(gate_and_score(&dat(Some(2000)), None, Some(2002), &[], &[], None, None, &[]), CandidateGate::Reject));
        let off = match gate_and_score(&dat(Some(2000)), None, Some(2001), &[], &[], None, None, &[]) {
            CandidateGate::Pass(s) => s,
            _ => panic!(),
        };
        let exact = match gate_and_score(&dat(Some(2000)), None, Some(2000), &[], &[], None, None, &[]) {
            CandidateGate::Pass(s) => s,
            _ => panic!(),
        };
        assert!(exact > off);
    }

    #[test]
    fn platform_gate() {
        assert!(matches!(gate_and_score(&dat(None), None, None, &[], &[], Some(&[3, 11]), Some(7), &[]), CandidateGate::Reject));
        assert!(matches!(gate_and_score(&dat(None), None, None, &[], &[], Some(&[3, 7]), Some(7), &[]), CandidateGate::Pass(_)));
    }

    #[test]
    fn variant_gate_and_compat() {
        let hack = ParsedName { variant: Some(Variant::Hack), ..ParsedName::default() };
        assert!(matches!(gate_and_score(&hack, Some(&ParsedName::default()), None, &[], &[], None, None, &[]), CandidateGate::Reject));
        let proto = ParsedName { variant: Some(Variant::Proto), ..ParsedName::default() };
        match gate_and_score(&proto, Some(&ParsedName::default()), None, &[], &[], None, None, &[]) {
            CandidateGate::Pass(s) => assert_eq!(s.variant_match, VariantMatch::Compatible),
            _ => panic!(),
        }
    }

    #[test]
    fn region_ordering_and_pick_best() {
        let parsed = ParsedName { regions: vec![RegionTag::Japan], ..ParsedName::default() };
        let dat_codes: Vec<&str> = parsed.regions.iter().flat_map(|r| r.all_codes()).collect();
        let score = |cand: &[&str]| match gate_and_score(&parsed, Some(&ParsedName::default()), None, cand, &["wor", "us", "eu"], None, None, &dat_codes) {
            CandidateGate::Pass(s) => s,
            _ => panic!(),
        };
        let exact = score(&["jp"]);
        let default = score(&["us"]);
        let other = score(&["br"]);
        assert!(exact.region_match > default.region_match && default.region_match > other.region_match);

        let a = 1u32;
        let b = 2u32;
        match pick_best([(&a, other), (&b, exact)]) {
            Selection::Best(c) => assert_eq!(*c, 2),
            _ => panic!(),
        }
        assert!(matches!(pick_best([(&a, exact), (&b, exact)]), Selection::Ambiguous));
        let empty: Vec<(&u32, CandidateScore)> = vec![];
        assert!(matches!(pick_best(empty), Selection::None));
    }
}
