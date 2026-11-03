//! Title normalization for comparison and indexed search. Ported from playmatch
//! `matching/util.rs` (MIT); unchanged in behavior apart from being `pub`.

use lazy_static::lazy_static;
use regex::Regex;
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// The base title with every parenthesised/bracketed tag group removed.
pub fn clean_name(input: &str) -> String {
    super::name_parse::parse_name(input).base
}

/// Folds a title down to a comparison form: trademark glyphs and diacritics stripped,
/// apostrophe variants unified, "&" -> "and", " - "/": " separators dropped, leading and
/// trailing articles (many locales) removed, roman numerals converted to arabic, whitespace
/// collapsed. Deliberately keeps case and punctuation like "'" (see the tests) -- callers
/// wanting a case-insensitive key lowercase the result.
pub fn normalize_title(input: &str) -> String {
    lazy_static! {
        static ref RE_AMPERSAND: Regex = Regex::new(r"\s*&\s*").unwrap();
        static ref RE_STRIP: Regex = Regex::new(r" - |: ").unwrap();
        static ref RE_LEADING: Regex = Regex::new(r"^(?i)(the |a |an |der |die |das |le |la |les |el |los |las |il |lo |gli |os |as )").unwrap();
        static ref RE_ARTICLE: Regex = Regex::new(r"(?i),\s*(the|a|an|der|die|das|le|la|les|el|los|las|il|lo|gli|os|as)\b").unwrap();
        static ref RE_ROMAN: Regex = Regex::new(r"\b(?i:M{0,4}(CM|CD|D?C{0,3})(XC|XL|L?X{0,3})(IX|IV|V?I{0,3}))\b").unwrap();
    }

    let pre = input.replace(['\u{2122}', '\u{00ae}', '\u{00a9}'], "");
    let mut s: String = pre.nfkd().filter(|c| !is_combining_mark(*c)).collect();
    s = s.replace(['\u{2018}', '\u{2019}', '\u{0060}'], "'");
    s = RE_AMPERSAND.replace_all(&s, " and ").to_string();
    s = RE_STRIP.replace_all(&s, " ").to_string();
    s = RE_LEADING.replace(&s, "").to_string();
    s = RE_ARTICLE.replace_all(&s, "").to_string();
    s = RE_ROMAN
        .replace_all(&s, |caps: &regex::Captures| {
            let roman = &caps[0];
            if !roman.is_empty() {
                if let Some(val) = roman_to_int(roman) {
                    return val.to_string();
                }
            }
            roman.to_string()
        })
        .to_string();
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Lowercased [`normalize_title`] of [`clean_name`] -- the key `release.norm_name` /
/// `title.norm_name` hold.
pub fn norm_key(input: &str) -> String {
    normalize_title(&clean_name(input)).to_lowercase()
}

/// The significant words of a name: [`norm_key`] split on anything not a letter / digit,
/// minus glue words and edition words that don't tell games apart.
fn significant_words(name: &str) -> Vec<String> {
    const SKIP: [&str; 22] = [
        "of", "the", "and", "a", "an", "to", "in", "on", "no", "wa", "ga", "de", "for", "edition", "version", "international", "collection", "special", "complete", "deluxe", "remastered", "hd",
    ];
    norm_key(name)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !SKIP.contains(w))
        .map(str::to_string)
        .collect()
}

/// Whether two names plausibly name the same game (or a version of it): their significant
/// words overlap by half (Jaccard), one's words all appear in the other ("Shin Legend of Hogehoge" /
/// "Legend of Hogehoge", "Hoge Quest VII International" / "Hoge Quest VII"), or -- for names
/// without spaces (Japanese) -- one contains the other. Used to catch wrong links, so it
/// errs on the side of "related".
pub fn names_related(a: &str, b: &str) -> bool {
    let (wa, wb) = (significant_words(a), significant_words(b));
    if wa.is_empty() || wb.is_empty() {
        let (ca, cb) = (norm_key(a).replace(' ', ""), norm_key(b).replace(' ', ""));
        return !ca.is_empty() && !cb.is_empty() && (ca.contains(&cb) || cb.contains(&ca));
    }
    let sa: std::collections::HashSet<&String> = wa.iter().collect();
    let sb: std::collections::HashSet<&String> = wb.iter().collect();
    // Sequel numbers must agree when both names have one ("VII" vs "VIII").
    let numbers = |s: &std::collections::HashSet<&String>| -> std::collections::HashSet<String> { s.iter().filter(|w| w.chars().all(|c| c.is_ascii_digit())).map(|w| w.to_string()).collect() };
    let (na, nb) = (numbers(&sa), numbers(&sb));
    if !na.is_empty() && !nb.is_empty() && na.is_disjoint(&nb) {
        return false;
    }
    let inter = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    let subset = inter == sa.len().min(sb.len()) && sa.len().min(sb.len()) >= 1 && sa.intersection(&sb).any(|w| w.chars().any(|c| c.is_alphabetic()));
    let (ca, cb) = (wa.concat(), wb.concat());
    subset
        || (inter * 2 >= union && inter > 0)
        || (ca.len() >= 4 && cb.len() >= 4 && (ca.contains(&cb) || cb.contains(&ca)))
        // A spelling variant ("Funpack" / "FunPak").
        || (ca.chars().count().min(cb.chars().count()) >= 6 && similarity(&ca, &cb) >= 0.85)
        // The same main title with a differently translated subtitle ("Code of Honor 2 -
        // Zasekrechennyj ostrov" / "Code of Honor 2: Conspiracy Island"), when the main
        // title is specific enough on its own.
        || {
            let (ma, mb) = (significant_words(main_title(a)), significant_words(main_title(b)));
            let specific = |m: &Vec<String>| m.len() >= 3 || (m.len() >= 2 && m.iter().any(|w| w.chars().all(|c| c.is_ascii_digit())));
            specific(&ma) && specific(&mb) && ma == mb
        }
}

/// Whether a search result's name is close enough to the name searched for to be linked
/// without an exact match (a "fuzzy" link). Much stricter than [`names_related`], which only
/// catches links that are plainly wrong: here the result must name *this* game.
///
/// - the same words, give or take punctuation, articles and word order (Jaccard of the
///   significant words at least 3/4), or
/// - every word searched for is in the result, and the result adds words only in its
///   subtitle ("Hoge Quest II" / "Hoge Quest II: The Subtitle"), or
/// - a spelling variant (`similarity` at least 0.85 on the joined words);
/// - and sequel numbers, where both have one, agree.
///
/// "Fuga Hoge" is not close to "Hoge": a word searched for is missing. Such a pair was linked
/// when an IGDB search scoped to the SNES missed a Japan-only game (filed under the Super
/// Famicom) and the first result -- another game sharing one word -- was taken.
pub fn names_close(query: &str, candidate: &str) -> bool {
    let (wq, wc) = (significant_words(query), significant_words(candidate));
    if wq.is_empty() || wc.is_empty() {
        let (q, c) = (norm_key(query).replace(' ', ""), norm_key(candidate).replace(' ', ""));
        return !q.is_empty() && q == c;
    }
    let sq: std::collections::HashSet<&String> = wq.iter().collect();
    let sc: std::collections::HashSet<&String> = wc.iter().collect();
    // Every number in the names must agree, a numeral glued to a word too ("Alpha2" / "Alpha3"):
    // a sequel is another game, however alike the rest of the name. A game sold under another
    // number (a VI released as III) is matched through the provider's alternative names.
    if numbers_of(query) != numbers_of(candidate) {
        return false;
    }
    let inter = sq.intersection(&sc).count();
    let union = sq.union(&sc).count();
    if inter * 4 >= union * 3 {
        return true;
    }
    let main: std::collections::HashSet<String> = significant_words(main_title(candidate)).into_iter().collect();
    if sq.is_subset(&sc) && !main.is_empty() && main.iter().all(|w| sq.contains(w)) {
        return true;
    }
    let (cq, cc) = (wq.concat(), wc.concat());
    cq.chars().count().min(cc.chars().count()) >= 6 && similarity(&cq, &cc) >= 0.85
}

/// Every run of digits in a name's comparison key (roman numerals already arabic), as numbers.
fn numbers_of(name: &str) -> std::collections::BTreeSet<u64> {
    norm_key(name).split(|c: char| !c.is_ascii_digit()).filter(|d| !d.is_empty()).filter_map(|d| d.parse().ok()).collect()
}

/// The part of a name before its subtitle (" - ", ": ", " – ").
fn main_title(name: &str) -> &str {
    [" - ", ": ", " \u{2013} "].iter().filter_map(|sep| name.find(sep)).min().map_or(name, |i| &name[..i])
}

/// Levenshtein similarity in 0..=1 (1 = equal), by characters.
fn similarity(a: &str, b: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let max = a.len().max(b.len());
    if max == 0 {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    1.0 - prev[b.len()] as f64 / max as f64
}

/// Convert a Roman numeral (up to 3999) to an integer.
pub(crate) fn roman_to_int(roman: &str) -> Option<u32> {
    let mut result = 0;
    let mut prev = 0;
    for c in roman.chars().rev() {
        let val = match c {
            'I' | 'i' => 1,
            'V' | 'v' => 5,
            'X' | 'x' => 10,
            'L' | 'l' => 50,
            'C' | 'c' => 100,
            'D' | 'd' => 500,
            'M' | 'm' => 1000,
            _ => return None,
        };
        if val < prev {
            result -= val;
        } else {
            result += val;
        }
        prev = val;
    }
    Some(result)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_fuzzy_link_must_name_the_same_game() {
        assert!(!names_close("Fuga Hoge", "Hoge"), "a word searched for is missing");
        assert!(!names_close("Hoge", "Fuga Hoge"), "the result's main title has a word more");
        assert!(names_close("Legend of Hogehoge, The - A Link to the Fuga", "The Legend of Hogehoge: A Link to the Fuga"));
        assert!(names_close("Hoge Quest II", "Hoge Quest II: The Subtitle"), "extra words in the subtitle only");
        assert!(!names_close("Hoge Quest III", "Hoge Quest VI"), "sequel numbers differ");
        assert!(!names_close("Hoge Quest", "Hoge Quest II"), "one has a sequel number");
        assert!(!names_close("Hoge Fantasy II", "Hoge Fantasy III"), "neighbouring numerals");
        assert!(!names_close("Hoge Fantasy 2", "Hoge Fantasy III"), "arabic against roman");
        assert!(!names_close("Hoge Fighter Alpha2", "Hoge Fighter Alpha3"), "a numeral glued to a word");
        assert!(names_close("Hoge Fantasy VI", "Hoge Fantasy 6"), "the same number written two ways");
        assert!(names_close("Hogemon Puzzle League", "Hogémon Puzzle League"));
    }

    use super::*;

    #[test]
    fn normalizes_articles_numerals_and_glyphs() {
        assert_eq!(normalize_title("The Example: Part IV"), "Example Part 4");
        assert_eq!(normalize_title("Example, The - Second Part"), "Example Second Part");
        assert_eq!(normalize_title("Café & Bar™"), "Cafe and Bar");
        assert_eq!(normalize_title("Rocky II"), "Rocky 2");
        assert_eq!(normalize_title("Der Herr der Ringe"), "Herr der Ringe");
        assert_eq!(normalize_title("Hoge Quest I"), "Hoge Quest 1");
        assert_eq!(normalize_title("O Holy Night"), "O Holy Night");
    }

    #[test]
    fn apostrophes_collapse() {
        assert_eq!(normalize_title("Majora\u{2019}s Mask"), normalize_title("Majora's Mask"));
    }

    #[test]
    fn related_names() {
        assert!(names_related("Hoge Quest VII International (Japan)", "Hoge Quest VII"));
        assert!(names_related("Shin Legend of Hogehoge II", "Legend of Hogehoge II"));
        assert!(names_related("Hoge Chronicle", "Hoge Chronicle (Europe)"));
        assert!(names_related("Altname 2", "Mainname 2: Altname 2"));
        assert!(names_related("ほげクエスト VII", "ほげクエスト VII: インターナショナル版"));
        assert!(!names_related("Stories of Sample", "Sound: Sound Creation for the Console"));
        assert!(!names_related("Otherfight", "Hoge Quest VII"));
        assert!(!names_related("Wip3out", "Stories of Sample"));
        assert!(!names_related("Hoge Quest VIII", "Hoge Quest VII"), "numbers matter");
        assert!(names_related("Code of Honor 2 - Zasekrechennyj ostrov", "Code of Honor 2: Conspiracy Island"));
        assert!(names_related("Casino Funpack", "Casino FunPak"));
        assert!(!names_related("Zak Ripp #04", "Zak McKracken and the Alien Mindbenders"));
        assert!(!names_related("Robot PD 16103", "Escape from the Planet of the Robot Monsters"));
        assert!(!names_related("Jet Grind Radio", "Garou: Mark of the Wolves"));
        assert!(!names_related("Iron Maiden Unfinished", "Iron Lord"));    }

    #[test]
    fn norm_key_strips_tags_and_lowercases() {
        assert_eq!(norm_key("The Example (USA) (Rev A)"), "example");
    }
}
