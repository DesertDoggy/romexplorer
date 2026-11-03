//! The name dictionary: one game or series under its names in different scripts -- "Hoge
//! Quest" and ホゲクエスト -- so a search in one finds the other (titles, releases, games, the
//! cross-platform list, audio), and audio can be tagged with the title it belongs to
//! whichever language its album is named in (migration 0055).
//!
//! Where the names come from (`name_dictionary.source`):
//! - `catalog`: each title's own name beside its localized and alternative names, when one
//!   of them is in another script (IGDB's Japanese titles, RetroAchievements' alt names).
//! - `series`: from those, the pairs whose names carry the same number -- "Hoge Quest 4" and
//!   ホゲクエスト4 give "Hoge Quest" and ホゲクエスト, which then finds every game of the series.
//! - `wikidata`: Wikidata's video games and video game series that have a Japanese name,
//!   with their English and alternative names. Downloaded only when asked; the download is
//!   kept beside the provider cache (zstd) so a rebuild without it needs no network.
//! - `user`: added by hand, never touched by a rebuild. An automatic name the user removes is
//!   recorded in `name_dictionary_blocks` and stays out of later rebuilds.
//!
//! Matching works on tokens: a name is split at spaces and punctuation and where Latin
//! letters meet Japanese script ("ホゲクエストIV" is ホゲクエスト + IV), each token folded
//! ([`tokenize`]). A run of query tokens that spells a dictionary name is swapped for that
//! name's counterparts in the other script; inside a Japanese token, which has no spaces to
//! split on, the longest dictionary name it starts with is.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use sqlx::{MySqlPool, Row};
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

use super::error::DatabaseError;
use super::jobs::Progress;

/// Hiragana, katakana, CJK ideographs, hangul. The katakana middle dot is a separator.
pub fn is_cjk(c: char) -> bool {
    c != '\u{30FB}'
        && matches!(c,
            '\u{3040}'..='\u{30FF}' | '\u{31F0}'..='\u{31FF}' | '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '\u{AC00}'..='\u{D7AF}' | '\u{3005}' | '\u{3006}')
}

/// One piece of a name: as written (after NFKC) and folded for matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub raw: String,
    pub norm: String,
    pub cjk: bool,
}

fn kana_fold(c: char) -> char {
    // Hiragana as katakana: ほげ and ホゲ are one name.
    if ('\u{3041}'..='\u{3096}').contains(&c) {
        char::from_u32(c as u32 + 0x60).unwrap_or(c)
    } else {
        c
    }
}

/// "iv" -> "4" for a whole token that is a well-formed roman numeral from 2 to 39 ("i" alone
/// is too often the word).
fn roman_token(s: &str) -> Option<String> {
    if s.len() < 2 || !s.chars().all(|c| matches!(c, 'i' | 'v' | 'x' | 'l')) {
        return None;
    }
    const ONES: [&str; 10] = ["", "i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix"];
    const TENS: [&str; 4] = ["", "x", "xx", "xxx"];
    (2..40).find(|n: &usize| format!("{}{}", TENS[n / 10], ONES[n % 10]) == s).map(|n| n.to_string())
}

fn token_norm(raw: &str, cjk: bool) -> String {
    if cjk {
        raw.chars().map(kana_fold).collect()
    } else {
        let s: String = raw.nfkd().filter(|c| !is_combining_mark(*c)).collect::<String>().to_lowercase();
        roman_token(&s).unwrap_or(s)
    }
}

/// Splits a name into [`Token`]s: at anything that is not a letter or digit, and where
/// Latin letters / digits meet Japanese script. Apostrophes are dropped without a split.
pub fn tokenize(input: &str) -> Vec<Token> {
    let text: String = input.nfkc().collect();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_cjk = false;
    let flush = |cur: &mut String, cjk: bool, out: &mut Vec<Token>| {
        if !cur.is_empty() {
            out.push(Token { norm: token_norm(cur, cjk), raw: std::mem::take(cur), cjk });
        }
    };
    for c in text.chars() {
        if matches!(c, '\'' | '\u{2019}' | '\u{2018}') {
            continue;
        }
        let class = if is_cjk(c) {
            Some(true)
        } else if c.is_alphanumeric() {
            Some(false)
        } else {
            None
        };
        match class {
            None => flush(&mut cur, cur_cjk, &mut out),
            Some(k) => {
                if !cur.is_empty() && k != cur_cjk {
                    flush(&mut cur, cur_cjk, &mut out);
                }
                cur_cjk = k;
                cur.push(c);
            }
        }
    }
    flush(&mut cur, cur_cjk, &mut out);
    out
}

/// The matching key of a whole name: its tokens' folded forms run together.
pub fn norm(input: &str) -> String {
    tokenize(input).into_iter().map(|t| t.norm).collect()
}

/// Whether a name has any Japanese (or other CJK) script in it.
pub fn has_cjk(input: &str) -> bool {
    input.chars().any(is_cjk)
}

// ---------------------------------------------------------------------------------------
// the dictionary in memory
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Term {
    raw: String,
    cjk: bool,
    /// Lower first: user, wikidata, catalog, series.
    rank: u8,
    /// The group's first name in its script: Wikidata's label (the rest are alternative
    /// names), a title's own name, its first localized one.
    primary: bool,
}

fn source_rank(source: &str) -> u8 {
    match source {
        "user" => 0,
        "wikidata" => 1,
        "catalog" => 2,
        _ => 3,
    }
}

/// The groups and an index from each name's key to the groups it is in (and whether it is
/// that group's main name there).
#[derive(Debug, Default)]
pub struct Dict {
    by_norm: HashMap<String, Vec<(u32, bool)>>,
    groups: Vec<Vec<Term>>,
}

/// One stretch of a query that the dictionary knows.
#[derive(Debug)]
struct Span {
    start: usize,
    end: usize,
    /// For a match at the start of a Japanese token: how many of its characters matched (the
    /// rest stays as it is).
    prefix: Option<usize>,
    alternates: Vec<String>,
}

impl Dict {
    /// Builds from `(group_key, term, source)` rows, each group's names in the order they were
    /// written (main names first).
    pub fn from_rows<'a>(rows: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>) -> Self {
        let mut index: HashMap<&str, u32> = HashMap::new();
        let mut dict = Dict::default();
        for (key, term, source) in rows {
            let g = *index.entry(key).or_insert_with(|| {
                dict.groups.push(Vec::new());
                (dict.groups.len() - 1) as u32
            });
            let n = norm(term);
            if n.chars().count() < 2 {
                continue;
            }
            let cjk = has_cjk(term);
            let primary = !dict.groups[g as usize].iter().any(|t| t.cjk == cjk);
            dict.groups[g as usize].push(Term { raw: term.to_string(), cjk, rank: source_rank(source), primary });
            let at = dict.by_norm.entry(n).or_default();
            if !at.iter().any(|(x, _)| *x == g) {
                at.push((g, primary));
            }
        }
        for g in &mut dict.groups {
            g.sort_by_key(|t| t.rank);
        }
        dict
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// The names in the other script for what `key` names: each group's main name there, at
    /// most `cap`, best source first. Groups whose main name `key` is win; an alternative
    /// name counts only when it is no group's main name -- Wikidata's alternative names are
    /// loose, and "Hoge" listed among some other game's would otherwise answer for Hoge.
    fn counterparts(&self, key: &str, cjk: bool, cap: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let hits: &[(u32, bool)] = self.by_norm.get(key).map(Vec::as_slice).unwrap_or(&[]);
        let main = hits.iter().any(|(_, primary)| *primary);
        let mut terms: Vec<&Term> = hits.iter().filter(|(_, primary)| *primary || !main).flat_map(|(g, _)| self.groups[*g as usize].iter()).filter(|t| t.cjk != cjk && t.primary).collect();
        terms.sort_by_key(|t| t.rank);
        for t in terms {
            if out.len() >= cap {
                break;
            }
            if seen.insert(norm(&t.raw)) {
                out.push(t.raw.clone());
            }
        }
        out
    }

    fn spans(&self, tokens: &[Token]) -> Vec<Span> {
        let mut spans = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            let mut found = None;
            for len in (1..=(tokens.len() - i).min(8)).rev() {
                let key: String = tokens[i..i + len].iter().map(|t| t.norm.as_str()).collect();
                if key.chars().count() < 2 || !self.by_norm.contains_key(&key) {
                    continue;
                }
                let cjk = tokens[i..i + len].iter().any(|t| t.cjk);
                let alternates = self.counterparts(&key, cjk, 3);
                if !alternates.is_empty() {
                    found = Some(Span { start: i, end: i + len, prefix: None, alternates });
                    break;
                }
            }
            if found.is_none() && tokens[i].cjk {
                let chars: Vec<char> = tokens[i].norm.chars().collect();
                for n in (2..chars.len()).rev() {
                    let key: String = chars[..n].iter().collect();
                    if self.by_norm.contains_key(&key) {
                        let alternates = self.counterparts(&key, true, 3);
                        if !alternates.is_empty() {
                            found = Some(Span { start: i, end: i + 1, prefix: Some(n), alternates });
                            break;
                        }
                    }
                }
            }
            match found {
                Some(span) => {
                    i = span.end;
                    spans.push(span);
                }
                None => i += 1,
            }
        }
        spans
    }

    /// `query` with what the dictionary knows swapped for its counterparts in the other
    /// script: every known stretch swapped at once, then each alternative of each stretch
    /// on its own. At most `cap`, never `query` itself.
    pub fn variants(&self, query: &str, cap: usize) -> Vec<String> {
        let tokens = tokenize(query);
        let spans = self.spans(&tokens);
        if spans.is_empty() {
            return Vec::new();
        }
        let render = |choice: &[Option<usize>]| -> String {
            let mut parts: Vec<String> = Vec::new();
            let mut i = 0;
            let mut s = 0;
            while i < tokens.len() {
                match spans.get(s).filter(|sp| sp.start == i) {
                    Some(sp) => {
                        match choice[s] {
                            Some(a) => {
                                parts.push(sp.alternates[a].clone());
                                if let Some(n) = sp.prefix {
                                    let rest: String = tokens[i].raw.chars().skip(n).collect();
                                    if !rest.is_empty() {
                                        parts.push(rest);
                                    }
                                }
                            }
                            None => parts.extend(tokens[sp.start..sp.end].iter().map(|t| t.raw.clone())),
                        }
                        i = sp.end;
                        s += 1;
                    }
                    None => {
                        parts.push(tokens[i].raw.clone());
                        i += 1;
                    }
                }
            }
            parts.join(" ")
        };
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::from([norm(query)]);
        let mut push = |v: String, out: &mut Vec<String>| {
            if out.len() < cap && seen.insert(norm(&v)) {
                out.push(v);
            }
        };
        push(render(&vec![Some(0); spans.len()]), &mut out);
        for (s, sp) in spans.iter().enumerate() {
            for a in 0..sp.alternates.len() {
                let mut choice = vec![None; spans.len()];
                choice[s] = Some(a);
                push(render(&choice), &mut out);
            }
        }
        out
    }

    /// Every group's names.
    pub fn groups(&self) -> impl Iterator<Item = Vec<String>> + '_ {
        self.groups.iter().map(|g| g.iter().map(|t| t.raw.clone()).collect())
    }

    /// Every name in the groups `name` is in, `name`'s own included (for the auto-tagger).
    pub fn equivalents(&self, name: &str) -> Vec<String> {
        let key = norm(name);
        let mut out = vec![name.to_string()];
        let mut seen = HashSet::from([key.clone()]);
        for (g, _) in self.by_norm.get(&key).into_iter().flatten() {
            for t in &self.groups[*g as usize] {
                if seen.insert(norm(&t.raw)) {
                    out.push(t.raw.clone());
                }
            }
        }
        out
    }
}

struct Cached {
    checked: Instant,
    stamp: (i64, u64),
    dict: Arc<Dict>,
}

static CACHE: RwLock<Option<Cached>> = RwLock::new(None);

/// The dictionary, loaded once and again only when its rows changed (looked at no more than
/// every half minute).
pub async fn dict(pool: &MySqlPool) -> Result<Arc<Dict>, DatabaseError> {
    if let Some(c) = CACHE.read().ok().as_ref().and_then(|c| c.as_ref()) {
        if c.checked.elapsed() < Duration::from_secs(30) {
            return Ok(Arc::clone(&c.dict));
        }
    }
    let row = sqlx::query("SELECT COUNT(*) AS n, CAST(COALESCE(MAX(id), 0) AS UNSIGNED) AS m FROM name_dictionary").fetch_one(pool).await?;
    let stamp: (i64, u64) = (row.try_get("n")?, row.try_get("m")?);
    if let Ok(mut guard) = CACHE.write() {
        if let Some(c) = guard.as_mut().filter(|c| c.stamp == stamp) {
            c.checked = Instant::now();
            return Ok(Arc::clone(&c.dict));
        }
    }
    let rows = sqlx::query("SELECT group_key, term, source FROM name_dictionary ORDER BY id").fetch_all(pool).await?;
    let triples: Vec<(String, String, String)> = rows.iter().map(|r| Ok((r.try_get("group_key")?, r.try_get("term")?, r.try_get("source")?))).collect::<Result<_, sqlx::Error>>()?;
    let dict = Arc::new(Dict::from_rows(triples.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))));
    if let Ok(mut guard) = CACHE.write() {
        *guard = Some(Cached { checked: Instant::now(), stamp, dict: Arc::clone(&dict) });
    }
    Ok(dict)
}

/// Drops the loaded dictionary (after an edit, so the next search sees it at once).
pub fn forget() {
    if let Ok(mut guard) = CACHE.write() {
        *guard = None;
    }
}

/// A LIKE pattern from a name's tokens: `%ホゲクエスト%4%` matches ホゲクエスト4 and
/// "ホゲ・クエスト IV" alike... as far as LIKE can: the tokens in order, anything between.
pub fn like_tokens(name: &str) -> String {
    let parts: Vec<String> = tokenize(name).into_iter().map(|t| t.raw.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")).collect();
    format!("%{}%", parts.join("%"))
}

/// The other ways to write `query` a search should also try, as LIKE patterns -- none when
/// the setting `search_translations` is off or the dictionary knows nothing in it.
pub async fn search_patterns(pool: &MySqlPool, query: &str) -> Result<Vec<String>, DatabaseError> {
    if !super::settings::search_translations(pool).await? {
        return Ok(Vec::new());
    }
    let dict = dict(pool).await?;
    Ok(dict.variants(query, 6).iter().map(|v| like_tokens(v)).collect())
}

/// A search condition over `columns` for `query` and its translations: each column LIKE the
/// query, or LIKE any of the variants. `binds` gets one pattern per placeholder, in order.
/// `columns` are SQL snippets with one `?` each (a column `LIKE ?`, or an EXISTS with one).
pub async fn search_condition(pool: &MySqlPool, query: &str, columns: &[&str], plain: String) -> Result<(String, Vec<String>), DatabaseError> {
    let mut patterns = vec![plain];
    patterns.extend(search_patterns(pool, query).await?);
    let mut parts = Vec::new();
    let mut binds = Vec::new();
    for p in &patterns {
        for c in columns {
            parts.push(c.to_string());
            binds.push(p.clone());
        }
    }
    Ok((format!("({})", parts.join(" OR ")), binds))
}

// ---------------------------------------------------------------------------------------
// building
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize)]
pub struct BuildSummary {
    pub catalog_groups: u64,
    pub series_groups: u64,
    pub wikidata_groups: u64,
    pub names: u64,
    pub blocked: u64,
    /// Whether this run downloaded Wikidata (else the kept download, if any, was used).
    pub wikidata_downloaded: bool,
    pub wikidata_note: Option<String>,
}

/// Where the Wikidata download is kept.
pub fn wikidata_cache(cache_dir: &Path) -> PathBuf {
    cache_dir.join("dictionary").join("wikidata.tsv.zst")
}

/// One group of names, deduplicated by key, Japanese and Latin both present.
fn group_of(names: impl IntoIterator<Item = String>) -> Option<Vec<String>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for n in names {
        let n = n.trim().to_string();
        let k = norm(&n);
        if n.is_empty() || n.chars().count() > 255 || k.chars().count() < 2 || !seen.insert(k) {
            continue;
        }
        out.push(n);
    }
    (out.iter().any(|n| has_cjk(n)) && out.iter().any(|n| !has_cjk(n))).then_some(out)
}

/// A numbered name taken apart: the words before the number, and the number ("Hoge Quest
/// IV: Fuga" -> ("Hoge Quest", 4)). None without a number after at least one word.
pub fn split_number(name: &str) -> Option<(String, String)> {
    let tokens = tokenize(name);
    let at = tokens.iter().position(|t| !t.cjk && t.norm.chars().all(|c| c.is_ascii_digit()))?;
    if at == 0 {
        return None;
    }
    let base: Vec<&str> = tokens[..at].iter().map(|t| t.raw.as_str()).collect();
    let sep = if tokens[..at].iter().all(|t| t.cjk) { "" } else { " " };
    Some((base.join(sep), tokens[at].norm.trim_start_matches('0').to_string()))
}

/// Series pairs out of title groups: a Latin and a Japanese name with the same number.
pub fn series_pairs(groups: &[Vec<String>]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for g in groups {
        let latin: Vec<(String, String)> = g.iter().filter(|n| !has_cjk(n)).filter_map(|n| split_number(n)).collect();
        let japanese: Vec<(String, String)> = g.iter().filter(|n| has_cjk(n)).filter_map(|n| split_number(n)).collect();
        for (lb, ln) in &latin {
            for (jb, jn) in &japanese {
                if ln == jn && has_cjk(jb) && norm(lb).chars().count() >= 3 && seen.insert((norm(lb), norm(jb))) {
                    out.push((lb.clone(), jb.clone()));
                }
            }
        }
    }
    out
}

/// Wikidata's TSV value (`"Hoge Quest"@en`, `<http://www.wikidata.org/entity/Q1>`) unwrapped.
fn tsv_value(v: &str) -> Option<String> {
    let v = v.trim();
    if let Some(rest) = v.strip_prefix('<') {
        return rest.strip_suffix('>').and_then(|u| u.rsplit('/').next()).map(str::to_string);
    }
    let body = v.strip_prefix('"')?;
    let end = body.rfind('"')?;
    Some(body[..end].replace("\\\"", "\"").replace("\\t", " ").replace("\\n", " ").replace("\\\\", "\\"))
}

/// Groups out of the Wikidata download: `item \t name` lines, several per item.
pub fn wikidata_groups(tsv: &str) -> Vec<(String, Vec<String>)> {
    let mut by_item: HashMap<String, Vec<String>> = HashMap::new();
    let mut order = Vec::new();
    for line in tsv.lines() {
        let mut cols = line.split('\t');
        let (Some(item), Some(rest)) = (cols.next().and_then(tsv_value), Some(cols)) else { continue };
        if !item.starts_with('Q') {
            continue;
        }
        // A name with a bracketed disambiguation ("Hoge (game)", "ホゲ (1990年)") is Wikidata
        // telling its own items apart, not a name anyone searches by.
        let names: Vec<String> = rest.filter_map(tsv_value).filter(|n| !n.is_empty() && !n.contains(['(', '（'])).collect();
        if names.is_empty() {
            continue;
        }
        let at = by_item.entry(item.clone()).or_insert_with(|| {
            order.push(item.clone());
            Vec::new()
        });
        at.extend(names);
    }
    order.into_iter().filter_map(|item| {
        let names = by_item.remove(&item)?;
        group_of(names.into_iter().take(12)).map(|g| (format!("wd:{item}"), g))
    }).collect()
}

/// The queries: video games and video game series (labels in Japanese and English), then
/// their alternative names in both. Split up because one query asking for all of it at once
/// runs past Wikidata's time limit; these take about fifteen seconds between them.
const WIKIDATA_QUERIES: [&str; 4] = [
    r#"SELECT ?item ?ja ?en WHERE { ?item wdt:P31 wd:Q7889 ; rdfs:label ?ja . FILTER(LANG(?ja) = "ja") OPTIONAL { ?item rdfs:label ?en . FILTER(LANG(?en) = "en") } }"#,
    r#"SELECT ?item ?ja ?en WHERE { ?item wdt:P31 wd:Q7058673 ; rdfs:label ?ja . FILTER(LANG(?ja) = "ja") OPTIONAL { ?item rdfs:label ?en . FILTER(LANG(?en) = "en") } }"#,
    r#"SELECT ?item ?alt WHERE { VALUES ?cls { wd:Q7889 wd:Q7058673 } ?item wdt:P31 ?cls ; skos:altLabel ?alt . FILTER(LANG(?alt) = "ja") }"#,
    r#"SELECT ?item ?alt WHERE { VALUES ?cls { wd:Q7889 wd:Q7058673 } ?item wdt:P31 ?cls ; rdfs:label ?ja ; skos:altLabel ?alt . FILTER(LANG(?ja) = "ja") FILTER(LANG(?alt) = "en") }"#,
];

async fn download_wikidata(http: &reqwest::Client, progress: &Progress) -> Result<String, DatabaseError> {
    let mut all = String::new();
    for (i, q) in WIKIDATA_QUERIES.iter().enumerate() {
        progress.report(i as u64, Some(WIKIDATA_QUERIES.len() as u64), &format!("Wikidata: query {} of {}", i + 1, WIKIDATA_QUERIES.len())).await;
        let body = http
            .get("https://query.wikidata.org/sparql")
            .query(&[("query", *q)])
            .header("Accept", "text/tab-separated-values")
            .header("User-Agent", concat!("romexplorer/", env!("CARGO_PKG_VERSION"), " (name dictionary)"))
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| DatabaseError::Parse(format!("Wikidata query {}: {e}", i + 1)))?
            .text()
            .await
            .map_err(|e| DatabaseError::Parse(format!("Wikidata query {}: {e}", i + 1)))?;
        // The header line of each answer is not an item and is skipped when parsed.
        all.push_str(&body);
        all.push('\n');
    }
    Ok(all)
}

/// Rebuilds the automatic names: from the catalogue, the series pairs in it, and Wikidata
/// (downloaded now when `download`, else the kept download if there is one). Names added by
/// hand stay; names the user removed stay out.
pub async fn rebuild(pool: &MySqlPool, http: &reqwest::Client, cache_dir: &Path, download: bool, progress: &Progress) -> Result<BuildSummary, DatabaseError> {
    let mut s = BuildSummary::default();
    progress.force(0, None, "dictionary: titles and their localized names").await;
    let mut names: HashMap<u64, Vec<String>> = HashMap::new();
    for sql in [
        "SELECT t.id, t.name FROM title t WHERE t.deleted = FALSE",
        "SELECT tl.title_id AS id, tl.name FROM title_locales tl JOIN title t ON t.id = tl.title_id AND t.deleted = FALSE",
        "SELECT tan.title_id AS id, tan.name FROM title_alt_names tan JOIN title t ON t.id = tan.title_id AND t.deleted = FALSE",
    ] {
        for r in sqlx::query(sql).fetch_all(pool).await? {
            names.entry(r.try_get("id")?).or_default().push(r.try_get("name")?);
        }
    }
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    let mut catalog: Vec<Vec<String>> = Vec::new();
    for (id, list) in names {
        if let Some(g) = group_of(list) {
            catalog.push(g.clone());
            groups.push((format!("title:{id}"), g));
        }
    }
    s.catalog_groups = groups.len() as u64;
    let mut sources: Vec<&str> = vec!["catalog"; groups.len()];
    for (latin, japanese) in series_pairs(&catalog) {
        groups.push((format!("series:{}", &super::import::sha1_hex(&norm(&latin))[..16]), vec![latin, japanese]));
        sources.push("series");
        s.series_groups += 1;
    }
    let cache = wikidata_cache(cache_dir);
    let tsv = if download {
        match download_wikidata(http, progress).await {
            Ok(tsv) => {
                if let Some(dir) = cache.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                // zstd -3, like everything else this project picks the format of.
                if let Err(e) = zstd::encode_all(tsv.as_bytes(), 3).map_err(|e| e.to_string()).and_then(|b| std::fs::write(&cache, b).map_err(|e| e.to_string())) {
                    log::warn!("dictionary: could not keep the Wikidata download at {}: {e}", cache.display());
                }
                s.wikidata_downloaded = true;
                Some(tsv)
            }
            Err(e) => {
                s.wikidata_note = Some(format!("download failed, the kept copy used if any: {e}"));
                None
            }
        }
    } else {
        None
    };
    let tsv = tsv.or_else(|| std::fs::read(&cache).ok().and_then(|b| zstd::decode_all(b.as_slice()).ok()).and_then(|b| String::from_utf8(b).ok()));
    if let Some(tsv) = &tsv {
        for (key, g) in wikidata_groups(tsv) {
            groups.push((key, g));
            sources.push("wikidata");
            s.wikidata_groups += 1;
        }
    } else if !download {
        s.wikidata_note.get_or_insert_with(|| "no Wikidata download kept yet".to_string());
    }

    progress.force(0, Some(groups.len() as u64), "dictionary: writing").await;
    let blocks: HashSet<(String, String)> = sqlx::query("SELECT group_key, norm FROM name_dictionary_blocks").fetch_all(pool).await?.iter().map(|r| Ok((r.try_get("group_key")?, r.try_get("norm")?))).collect::<Result<_, sqlx::Error>>()?;
    sqlx::query("DELETE FROM name_dictionary WHERE source IN ('catalog', 'series', 'wikidata')").execute(pool).await?;
    let mut rows: Vec<(String, String, String, bool, &str)> = Vec::new();
    for ((key, g), source) in groups.iter().zip(&sources) {
        for name in g {
            let k = norm(name);
            if blocks.contains(&(key.clone(), k.clone())) {
                s.blocked += 1;
                continue;
            }
            rows.push((key.clone(), name.clone(), k, has_cjk(name), source));
        }
    }
    for (n, chunk) in rows.chunks(1000).enumerate() {
        let sql = format!("INSERT IGNORE INTO name_dictionary (group_key, term, norm, cjk, source) VALUES {}", vec!["(?, ?, ?, ?, ?)"; chunk.len()].join(", "));
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for (key, term, k, cjk, source) in chunk {
            q = q.bind(key).bind(term).bind(k).bind(cjk).bind(*source);
        }
        s.names += q.execute(pool).await?.rows_affected();
        progress.report((n * 1000) as u64, Some(rows.len() as u64), "dictionary: writing").await;
    }
    forget();
    progress
        .force(1, Some(1), &format!("done: {} name(s) -- {} title group(s), {} series, {} from Wikidata{}", s.names, s.catalog_groups, s.series_groups, s.wikidata_groups, s.wikidata_note.as_deref().map(|n| format!(" ({n})")).unwrap_or_default()))
        .await;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fold_across_width_kana_case_and_numerals() {
        assert_eq!(norm("Hoge Quest IV"), "hogequest4");
        assert_eq!(norm("ﾎｹﾞｸｴｽﾄⅣ"), "ホゲクエスト4", "half-width kana and the numeral sign");
        assert_eq!(norm("ほげくえすと"), "ホゲクエスト", "hiragana as katakana");
        assert_eq!(norm("ホゲ・クエスト"), "ホゲクエスト");
        assert_eq!(norm("Hogé's Quest"), "hogesquest");
        let t = tokenize("ホゲクエストIV");
        assert_eq!(t.iter().map(|t| t.raw.as_str()).collect::<Vec<_>>(), ["ホゲクエスト", "IV"], "split where the scripts meet");
        assert_eq!(norm("I Am Hoge"), "iamhoge", "a lone I is a word");
    }

    fn sample() -> Dict {
        Dict::from_rows([
            ("series:a", "Hoge Quest", "series"),
            ("series:a", "ホゲクエスト", "series"),
            ("wd:Q1", "Fuga Fantasy", "wikidata"),
            ("wd:Q1", "フガファンタジー", "wikidata"),
            ("title:9", "Fuga Fantasy 4", "catalog"),
            ("title:9", "フガファンタジーIV", "catalog"),
        ])
    }

    #[test]
    fn a_query_in_one_script_gets_the_other() {
        let d = sample();
        assert_eq!(d.variants("ホゲクエスト", 6), vec!["Hoge Quest".to_string()]);
        assert_eq!(d.variants("hoge quest", 6), vec!["ホゲクエスト".to_string()]);
        // The longest stretch wins: the whole numbered title, then the series inside it.
        assert_eq!(d.variants("Fuga Fantasy IV", 6), vec!["フガファンタジーIV".to_string()]);
        assert_eq!(d.variants("Fuga Fantasy Tactics", 6), vec!["フガファンタジー Tactics".to_string()]);
        // Inside a Japanese token with no spaces, the name it starts with.
        assert_eq!(d.variants("フガファンタジータクティクス", 6), vec!["Fuga Fantasy タクティクス".to_string()]);
        assert!(d.variants("Unrelated", 6).is_empty());
        assert_eq!(like_tokens("フガファンタジー Tactics"), "%フガファンタジー%Tactics%");
    }

    #[test]
    fn a_main_name_beats_an_alternative_one() {
        let d = Dict::from_rows([
            ("wd:Q1", "ホゲ", "wikidata"),
            ("wd:Q1", "Hoge", "wikidata"),
            // Another item that lists "Hoge" among its alternative names.
            ("wd:Q2", "ピヨの人", "wikidata"),
            ("wd:Q2", "Piyo Person", "wikidata"),
            ("wd:Q2", "Hoge", "wikidata"),
            ("wd:Q3", "モゲ", "wikidata"),
            ("wd:Q3", "Moge", "wikidata"),
            ("wd:Q3", "Moge Classic", "wikidata"),
        ]);
        assert_eq!(d.variants("Hoge", 6), vec!["ホゲ".to_string()]);
        assert_eq!(d.variants("Moge Classic", 6), vec!["モゲ".to_string()], "an alternative name that is no group's main one still counts");
    }

    #[test]
    fn series_come_from_numbered_pairs() {
        let groups = vec![vec!["Hoge Quest IV".to_string(), "ホゲクエストIV".to_string()], vec!["Hoge Quest 2: Fuga".to_string(), "ホゲクエスト2".to_string()], vec!["Piyo".to_string(), "ピヨ3".to_string()]];
        assert_eq!(series_pairs(&groups), vec![("Hoge Quest".to_string(), "ホゲクエスト".to_string())]);
        assert_eq!(split_number("Hoge Quest IV: Fuga"), Some(("Hoge Quest".to_string(), "4".to_string())));
        assert_eq!(split_number("4 Hoge"), None);
    }

    #[test]
    fn wikidata_lines_group_by_item() {
        let tsv = "?item\t?ja\t?en\n<http://www.wikidata.org/entity/Q5>\t\"ホゲクエスト\"@ja\t\"Hoge Quest\"@en\n<http://www.wikidata.org/entity/Q6>\t\"Latin Only\"@ja\t\n?item\t?alt\n<http://www.wikidata.org/entity/Q5>\t\"ほげくえ\"@ja\n<http://www.wikidata.org/entity/Q5>\t\"ホゲ (ゲーム)\"@ja\n";
        let g = wikidata_groups(tsv);
        assert_eq!(g, vec![("wd:Q5".to_string(), vec!["ホゲクエスト".to_string(), "Hoge Quest".to_string(), "ほげくえ".to_string()])]);
    }
}
