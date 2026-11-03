//! What a patched ROM's name says about it.
//!
//! Some patches include the release they were made from, followed by tags for the patch:
//! `Game (Japan) [T-En by Someone v1.0] [i]`, `Game (U) [T+Bra1.0_Someone]`,
//! `Game - Balance (V3.1) (Hack)`. Three things are read out of that here, for the DAT
//! generator ([`super::dat_convert`]) and the import alike:
//!
//! - **what it is**: a translation or a hack;
//! - **its language**, when it is a translation, and whether the name states it (a code in
//!   the tag, or "Spanish Translation") or it is only a guess from a loose word;
//! - **the work and the version**. Every version of one patch is one release holding several
//!   ROMs -- `[T-En by Someone v1.0]` and `[... v1.02]` are the same work -- so the version
//!   is taken out of the name and kept as the ROM's variant. Otherwise each version would be
//!   a release of its own and the list would be several times its real size.

use lazy_static::lazy_static;
use regex::Regex;

use crate::common::models::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchKind {
    Hack,
    Translation,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PatchInfo {
    pub kind: Option<PatchKind>,
    /// The language it is translated into (`Language::code`), and whether the name states it.
    pub language: Option<(String, bool)>,
    /// The name with every version taken out: what all versions of the patch share.
    pub work: String,
    /// What was taken out, in order ("v1.0 [i]"). `None` when the name has no version.
    pub variant: Option<String>,
    /// The original's name when the patch is a bracket tag after it (`Game (Japan)`): the
    /// release it was made from, by its exact DAT name. `None` when the name does not say.
    pub base: Option<String>,
}

lazy_static! {
    /// A group that is nothing but a version: `(V2)`, `(v1.02)`, `(ver 1.3b)`, `(Version 2)`.
    static ref RE_VERSION_GROUP: Regex = Regex::new(r"(?i)^(?:v|ver\.?|version)\s*\d[\w.\- ]*$").unwrap();
    /// `(Beta 3)`, `(Alpha)`, `(Demo 2)`, `(Final)`: a stage of a patch, when it follows one.
    static ref RE_STAGE_GROUP: Regex = Regex::new(r"(?i)^(?:beta|alpha|demo|final|wip|preview|rc)\s*[\d.]*[a-z]?$").unwrap();
    /// A date on its own: `[2020-01-31]`, `(2020-01-31)`, `(20200131)`.
    static ref RE_DATE_GROUP: Regex = Regex::new(r"^(?:\d{4}-\d{1,2}-\d{1,2}|\d{8})$").unwrap();
    /// A version inside a tag: ` v1.0`, ` Beta 1`, ` 1.02b` at the end, `v0.3`.
    static ref RE_VERSION_INSIDE: Regex = Regex::new(r"(?i)\s+(?:v(?:er(?:sion)?)?\.?\s*\d[\w.]*|(?:beta|alpha|rc)\s*\d[\w.]*|\d+(?:\.\d+)+[a-z]?)\b").unwrap();
    /// GoodTools' translation tag: `T+Bra1.0_Someone`, `T-Ara0.05a_Someone`, `T+Eng`.
    static ref RE_GOOD_T: Regex = Regex::new(r"^T[+-]([A-Za-z]{2,4})(\d[\d.]*[a-z]?)?(?:[_ ](.*))?$").unwrap();
    /// "Spanish Translation", "English translation".
    static ref RE_TRANSLATION_PHRASE: Regex = Regex::new(r"(?i)\b([A-Za-z]+)[ -]+translat(?:ion|ed)\b").unwrap();
    static ref RE_SPACES: Regex = Regex::new(r"\s{2,}").unwrap();
}

/// A language code as translation tags write them: ISO codes, and GoodTools' three letters.
pub fn language_of_tag(code: &str) -> Option<&'static str> {
    let lower = code.to_ascii_lowercase();
    let known = match lower.as_str() {
        "eng" => "en",
        "spa" | "esp" => "es",
        "fre" | "fra" => "fr",
        "ger" | "deu" => "de",
        "ita" => "it",
        "por" | "bra" | "br" => "pt",
        "rus" => "ru",
        "chi" | "chs" | "cht" | "zho" => "zh",
        "kor" => "ko",
        "jap" | "jpn" => "ja",
        "ara" => "ar",
        "pol" => "pl",
        "dut" | "nld" => "nl",
        "swe" => "sv",
        "dan" => "da",
        "nor" => "no",
        "fin" => "fi",
        "gre" | "ell" => "el",
        "tur" => "tr",
        "hun" => "hu",
        "cze" | "ces" => "cs",
        "cat" => "ca",
        "tha" => "th",
        "ind" => "id",
        "vie" => "vi",
        "heb" => "he",
        "ukr" => "uk",
        "rom" | "ron" | "rum" => "ro",
        "cro" | "hrv" => "hr",
        "alb" => "sq",
        "lux" => "lb",
        "bul" => "bg",
        other => {
            let by_code = Language::from_code(other);
            return (other.len() == 2 && by_code != Language::Other).then(|| by_code.code());
        }
    };
    Some(known)
}

/// A language by its English name ("Spanish", "Brazilian"): the catalogue's own languages,
/// and the ones translations are made into that it has no entry for.
pub fn language_of_word(word: &str) -> Option<&'static str> {
    if let Some(known) = Language::from_name(word).filter(|l| *l != Language::Other) {
        return Some(known.code());
    }
    Some(match word.trim().to_ascii_lowercase().as_str() {
        "english" => "en",
        "spanish" | "castilian" => "es",
        "french" => "fr",
        "german" => "de",
        "italian" => "it",
        "portuguese" | "brazilian" | "brazil" => "pt",
        "russian" => "ru",
        "chinese" => "zh",
        "korean" => "ko",
        "japanese" => "ja",
        "arabic" => "ar",
        "polish" => "pl",
        "dutch" => "nl",
        "swedish" => "sv",
        "danish" => "da",
        "norwegian" => "no",
        "finnish" => "fi",
        "greek" => "el",
        "turkish" => "tr",
        "hungarian" => "hu",
        "czech" => "cs",
        "catalan" => "ca",
        "thai" => "th",
        "indonesian" => "id",
        "vietnamese" => "vi",
        "hebrew" => "he",
        "ukrainian" => "uk",
        "romanian" => "ro",
        "bulgarian" => "bg",
        "serbian" => "sr",
        "croatian" => "hr",
        "galician" => "gl",
        "basque" => "eu",
        "latin" => "la",
        "malay" => "ms",
        "persian" => "fa",
        "lithuanian" => "lt",
        "slovak" => "sk",
        "slovenian" => "sl",
        _ => return None,
    })
}

/// One `(..)` or `[..]` group of a name: where it is, and its inside.
struct Group<'a> {
    start: usize,
    end: usize,
    bracket: bool,
    inner: &'a str,
}

fn groups(name: &str) -> Vec<Group<'_>> {
    let mut out = Vec::new();
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let close = match bytes[i] {
            b'(' => b')',
            b'[' => b']',
            _ => {
                i += 1;
                continue;
            }
        };
        match name[i + 1..].find(close as char) {
            Some(len) => {
                out.push(Group { start: i, end: i + len + 2, bracket: close == b']', inner: &name[i + 1..i + 1 + len] });
                i += len + 2;
            }
            None => break,
        }
    }
    out
}

/// A translation tag's language, the tag without its version, and the version.
fn translation_tag(inner: &str) -> Option<(Option<&'static str>, String, Option<String>)> {
    let trimmed = inner.trim();
    // No-Intro style: "T-En by Someone v1.0", "T-Es".
    let rest = trimmed.strip_prefix("T-").or_else(|| trimmed.strip_prefix("T+")).or_else(|| trimmed.strip_prefix("t-"))?;
    let code: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    if code.is_empty() || code.len() > 4 {
        return None;
    }
    if let Some(m) = RE_GOOD_T.captures(trimmed).filter(|m| m.get(2).is_some() || m.get(3).is_some_and(|by| !by.as_str().to_ascii_lowercase().starts_with("by "))) {
        // GoodTools: the version is glued to the code.
        let version = m.get(2).map(|v| v.as_str().to_string());
        let by = m.get(3).map(|b| b.as_str().trim()).filter(|b| !b.is_empty());
        let tag = match by {
            Some(by) => format!("{}{}_{by}", &trimmed[..2], &m[1]),
            None => format!("{}{}", &trimmed[..2], &m[1]),
        };
        return Some((language_of_tag(&m[1]), tag, version));
    }
    let version: Vec<String> = RE_VERSION_INSIDE.find_iter(trimmed).map(|m| m.as_str().trim().to_string()).collect();
    let tag = RE_VERSION_INSIDE.replace_all(trimmed, "").trim().to_string();
    Some((language_of_tag(&code), tag, (!version.is_empty()).then(|| version.join(" "))))
}

fn is_hack_tag(inner: &str) -> bool {
    let lower = inner.trim().to_ascii_lowercase();
    lower == "hack" || lower == "h" || lower.ends_with(" hack") || lower.starts_with("hack by ") || lower.starts_with("hack ") || lower.starts_with("h ")
        // The original voices put back: "(Undub)", "(Undub - Original Music)", "[Undub by X v1.1]".
        || lower == "undub"
        || lower.starts_with("undub ")
}

/// Reads a patched ROM's name (see the module doc). A name with no patch tag comes back
/// with `kind: None` and `work` equal to the name.
pub fn analyze(name: &str) -> PatchInfo {
    let name = name.trim();
    let all = groups(name);
    let mut info = PatchInfo::default();
    // (start, end, what stays in its place, what left) for each part taken out of the name.
    let mut cuts: Vec<(usize, usize, String, String)> = Vec::new();
    let mut first_patch: Option<usize> = None;
    let mut loose_language: Option<&'static str> = None;

    for (i, g) in all.iter().enumerate() {
        let inner = g.inner.trim();
        if g.bracket {
            if let Some((language, tag, version)) = translation_tag(inner) {
                info.kind = Some(PatchKind::Translation);
                if info.language.is_none() {
                    info.language = language.map(|l| (l.to_string(), true));
                }
                if let Some(v) = version {
                    cuts.push((g.start, g.end, format!("[{tag}]"), v));
                }
                first_patch.get_or_insert(i);
                continue;
            }
        }
        if is_hack_tag(inner) {
            if info.kind.is_none() {
                info.kind = Some(PatchKind::Hack);
            }
            first_patch.get_or_insert(i);
            continue;
        }
        let lower = inner.to_ascii_lowercase();
        if lower == "translated" || lower.starts_with("translated ") {
            info.kind = Some(PatchKind::Translation);
            if info.language.is_none() {
                info.language = lower.strip_prefix("translated ").and_then(|l| language_of_tag(l.trim()).or_else(|| language_of_word(l.trim()))).map(|l| (l.to_string(), true));
            }
            first_patch.get_or_insert(i);
            continue;
        }
        if loose_language.is_none() && !lower.contains(',') {
            // "[Spanish]", "(Russian)": a word, not a statement. A region list is not one.
            loose_language = language_of_word(inner).filter(|_| g.bracket || lower != "japan");
        }
    }

    if let Some(m) = RE_TRANSLATION_PHRASE.captures(name) {
        if let Some(language) = language_of_word(&m[1]) {
            info.kind = Some(PatchKind::Translation);
            info.language = Some((language.to_string(), true));
        } else if info.kind.is_none() {
            info.kind = Some(PatchKind::Translation);
        }
    }
    if info.kind.is_some() && info.language.is_none() {
        info.language = loose_language.map(|l| (l.to_string(), false));
    }

    if info.kind.is_some() {
        // A bracket tag after the original's name: everything before it is the original.
        if let Some(first) = first_patch.filter(|&i| all[i].bracket) {
            let base = name[..all[first].start].trim();
            info.base = (!base.is_empty()).then(|| base.to_string());
        }
        let patched_in_parens = first_patch.is_some_and(|i| !all[i].bracket);
        for (i, g) in all.iter().enumerate() {
            if cuts.iter().any(|c| c.0 == g.start) || Some(i) == first_patch {
                continue;
            }
            let inner = g.inner.trim();
            let after_patch = first_patch.is_some_and(|first| i > first);
            // After the patch tag every group describes the patch's build: its version, its
            // stage, its date, its flags ("[i]", "[US title theme]"). Before a bracket tag
            // it describes the original ("(v1.1)" is the cartridge's revision) and stays;
            // a hack named in parentheses has no such boundary, so a plain version counts.
            let cut = if after_patch { translation_tag(inner).is_none() && !is_hack_tag(inner) } else { patched_in_parens && (RE_VERSION_GROUP.is_match(inner) || RE_DATE_GROUP.is_match(inner)) };
            if cut {
                // A bare version reads better without its parentheses; a flag keeps its own.
                let bare = RE_VERSION_GROUP.is_match(inner) || RE_STAGE_GROUP.is_match(inner) || RE_DATE_GROUP.is_match(inner);
                cuts.push((g.start, g.end, String::new(), if bare { inner.to_string() } else { name[g.start..g.end].to_string() }));
            }
        }
    }

    cuts.sort_by_key(|c| c.0);
    let mut work = String::with_capacity(name.len());
    let mut at = 0;
    for (start, end, replacement, _) in &cuts {
        work.push_str(&name[at..*start]);
        work.push_str(replacement);
        at = *end;
    }
    work.push_str(&name[at..]);
    info.work = RE_SPACES.replace_all(work.trim(), " ").into_owned();
    info.variant = (!cuts.is_empty()).then(|| cuts.iter().map(|c| c.3.as_str()).collect::<Vec<_>>().join(" "));
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(name: &str) -> PatchInfo {
        analyze(name)
    }

    #[test]
    fn a_translation_tag_gives_the_language_the_work_and_the_version() {
        let i = a("Chocobo Land - A Game of Dice (Japan) [T-En by Nezz v1.0]");
        assert_eq!(i.kind, Some(PatchKind::Translation));
        assert_eq!(i.language, Some(("en".to_string(), true)));
        assert_eq!(i.work, "Chocobo Land - A Game of Dice (Japan) [T-En by Nezz]");
        assert_eq!(i.variant.as_deref(), Some("v1.0"));
        assert_eq!(i.base.as_deref(), Some("Chocobo Land - A Game of Dice (Japan)"));
        // Every version, and every build flag, is the same work.
        let b = a("Chocobo Land - A Game of Dice (Japan) [T-En by Nezz v1.02] [i]");
        assert_eq!(b.work, i.work);
        assert_eq!(b.variant.as_deref(), Some("v1.02 [i]"));
        let c = a("Crash Nitro Kart (Japan) [T-En by Cornholio309 v3.5] [US title theme] [n]");
        assert_eq!(c.work, "Crash Nitro Kart (Japan) [T-En by Cornholio309]");
        assert_eq!(c.variant.as_deref(), Some("v3.5 [US title theme] [n]"));
        // No version: nothing to take out.
        let d = a("Bomberman Jetters (Japan) [T-En by Some Project]");
        assert_eq!((d.work.as_str(), d.variant), ("Bomberman Jetters (Japan) [T-En by Some Project]", None));
        assert_eq!(a("Final Fire Pro (Japan) [T-En by Fat Dave Beta 1] [i]").variant.as_deref(), Some("Beta 1 [i]"));
    }

    #[test]
    fn the_originals_own_revision_stays_with_the_original() {
        let i = a("Game (USA) (v1.1) [T-Es by Someone v2.0]");
        assert_eq!(i.base.as_deref(), Some("Game (USA) (v1.1)"));
        assert_eq!(i.work, "Game (USA) (v1.1) [T-Es by Someone]");
        assert_eq!(i.variant.as_deref(), Some("v2.0"));
        assert_eq!(i.language, Some(("es".to_string(), true)));
    }

    #[test]
    fn goodtools_tags_are_read_too() {
        let i = a("Batman - Rise of Sin Tzu (U) (M3) [T+Bra1.0_MestreSplinter]");
        assert_eq!(i.kind, Some(PatchKind::Translation));
        assert_eq!(i.language, Some(("pt".to_string(), true)));
        assert_eq!(i.work, "Batman - Rise of Sin Tzu (U) (M3) [T+Bra_MestreSplinter]");
        assert_eq!(i.variant.as_deref(), Some("1.0"));
        let j = a("Pokemon - Ruby Version (U) (V1.0) [T-Ara0.05a_ExtraOrdinary]");
        assert_eq!((j.language.clone().map(|l| l.0).as_deref(), j.variant.as_deref()), (Some("ar"), Some("0.05a")));
        assert_eq!(j.base.as_deref(), Some("Pokemon - Ruby Version (U) (V1.0)"), "the cartridge's own version is the original's");
        assert_eq!(a("Army Men Advance (UE) (M5) [T+Bra_Evil Darkness]").variant, None);
    }

    #[test]
    fn a_hack_named_in_parentheses_loses_its_version() {
        let v2 = a("Advance Wars 2 - Black Hole Rising - Balance (V2) (Hack)");
        let v31 = a("Advance Wars 2 - Black Hole Rising - Balance (V3.1) (Hack)");
        assert_eq!(v2.kind, Some(PatchKind::Hack));
        assert_eq!(v2.work, "Advance Wars 2 - Black Hole Rising - Balance (Hack)");
        assert_eq!(v2.work, v31.work);
        assert_eq!((v2.variant.as_deref(), v31.variant.as_deref()), (Some("V2"), Some("V3.1")));
        assert_eq!(v2.base, None, "the name does not say which release it was made from");
        assert_eq!(a("Super Mario World - Kaizo (SMW Hack)").kind, Some(PatchKind::Hack));
        assert_eq!(a("Game (USA) [Hack by Someone v1.2]").kind, Some(PatchKind::Hack));
    }

    #[test]
    fn a_stated_language_is_confirmed_and_a_loose_word_is_a_guess() {
        assert_eq!(a("Game (Japan) (Translated En)").language, Some(("en".to_string(), true)));
        assert_eq!(a("Some Game - Spanish Translation (Hack)").language, Some(("es".to_string(), true)));
        assert_eq!(a("Some Game [Spanish] (Hack)").language, Some(("es".to_string(), false)));
        assert_eq!(a("Some Game (Hack)").language, None);
    }

    #[test]
    fn an_ordinary_name_is_left_alone() {
        for name in ["Game (USA) (v1.1)", "Game (Japan) (Beta 2)", "Game (Europe) (En,Fr,De) (Rev 1)", "Theme Park (USA)"] {
            let i = a(name);
            assert_eq!((i.kind, i.work.as_str(), i.variant, i.base), (None, name, None, None), "{name}");
        }
    }

    #[test]
    fn language_codes() {
        assert_eq!(language_of_tag("En"), Some("en"));
        assert_eq!(language_of_tag("Eng"), Some("en"));
        assert_eq!(language_of_tag("Bra"), Some("pt"));
        assert_eq!(language_of_tag("Chs"), Some("zh"));
        assert_eq!(language_of_tag("Xx"), None);
    }
}
