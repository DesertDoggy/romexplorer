//! Audio auto-tagging (`audio_autotag` job, run from the Audio tab): each album -- or, for
//! files with no album tag, the folder they are in -- is matched by name to a title, its
//! localized and alternative names, and the name dictionary's other-language versions
//! (`database::dictionary`), so ホゲクエスト オリジナル・サウンドトラック finds "Hoge Quest".
//! A match tags the files with the title's name and gives the title the same keyword, so
//! its page plays them; both are recorded as automatic (`audio_file_tags.source = 'auto'`,
//! `title_keywords.source = 'auto'`), to be filtered and removed apart from the user's own.
//!
//! A file the user tagged is never touched. Each run recomputes the automatic tags of the
//! rest; an automatic title keyword no file carries any more is removed at the end.
//!
//! The match is deliberately strict: the album name with what soundtracks add to a game's
//! name ("Original Soundtrack", "Vol. 2", サウンドトラック) taken off the end must spell a
//! title's name, or start with one that covers at least half of it. A name that belongs to
//! two different games is left alone rather than guessed.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;
use sqlx::Row;

use crate::database::dictionary::{self, Token};
use crate::database::jobs::Progress;

use super::{App, Result};

#[derive(Debug, Clone, Default, Serialize)]
pub struct AutotagSummary {
    /// Albums (or folders) looked at, and how many matched a title.
    pub albums: u64,
    pub matched: u64,
    /// Albums whose name fits two different games: left untagged.
    pub ambiguous: u64,
    pub files_tagged: u64,
    pub titles_tagged: u64,
    /// Files skipped because the user tagged them.
    pub skipped_manual: u64,
    /// Automatic title keywords removed because no file carries them any more.
    pub stale_title_tags: u64,
}

/// Words a soundtrack's name adds after the game's.
const NOISE: [&str; 41] = [
    "original", "soundtrack", "soundtracks", "sound", "sounds", "track", "tracks", "ost", "osv", "version", "music", "musics", "collection", "complete", "bgm", "arrange", "arranged", "album", "vol",
    "volume", "disc", "disk", "cd", "best", "selection", "selections", "plus", "game", "edition", "サウンドトラック", "オリジナル", "サウンド", "ミュージック", "コレクション", "アルバム", "ベスト", "サントラ", "音楽集",
    "アレンジ", "ヴァージョン", "バージョン",
];

/// What comes before a disc or volume number.
const NUMBERED: [&str; 5] = ["vol", "volume", "disc", "disk", "cd"];

/// The noise taken off the end of a name: whole tokens, and noise words a Japanese token
/// ends with (it has no spaces to split them off). A number goes only after vol / disc --
/// "Hoge Quest 2 Original Soundtrack" keeps its 2.
pub fn strip_noise(mut tokens: Vec<Token>) -> Vec<Token> {
    loop {
        let Some(last) = tokens.last() else { break };
        if NOISE.contains(&last.norm.as_str()) {
            tokens.pop();
            continue;
        }
        if last.norm.chars().all(|c| c.is_ascii_digit()) && tokens.len() >= 2 && NUMBERED.contains(&tokens[tokens.len() - 2].norm.as_str()) {
            tokens.truncate(tokens.len() - 2);
            continue;
        }
        if last.cjk {
            if let Some(word) = NOISE.iter().filter(|w| w.chars().any(dictionary::is_cjk)).find(|w| last.norm.ends_with(*w) && last.norm.chars().count() > w.chars().count()) {
                let keep = last.norm.chars().count() - word.chars().count();
                let t = tokens.last_mut().expect("checked above");
                t.raw = t.raw.chars().take(keep).collect();
                t.norm = t.norm.chars().take(keep).collect();
                continue;
            }
        }
        break;
    }
    tokens
}

fn key(tokens: &[Token]) -> String {
    tokens.iter().map(|t| t.norm.as_str()).collect()
}

/// A title name's keys: the whole name, and the part before a subtitle (": ", " - ").
fn name_keys(name: &str) -> Vec<(String, bool)> {
    let mut out = vec![(dictionary::norm(name), true)];
    for sep in [": ", " - ", "："] {
        if let Some((head, _)) = name.split_once(sep) {
            let k = dictionary::norm(head);
            if k.chars().count() >= 4 {
                out.push((k, false));
            }
        }
    }
    out
}

/// Titles by name key. Each entry: (title id, whether the key is the title's own whole name).
#[derive(Default)]
pub struct TitleIndex {
    by_key: HashMap<String, Vec<(u64, bool)>>,
    names: HashMap<u64, String>,
}

impl TitleIndex {
    pub fn add(&mut self, title: u64, name: &str, own: bool) {
        for (k, whole) in name_keys(name) {
            if k.chars().count() < 3 {
                continue;
            }
            let at = self.by_key.entry(k).or_default();
            if !at.iter().any(|(t, _)| *t == title) {
                at.push((title, own && whole));
            }
        }
    }

    pub fn set_name(&mut self, title: u64, name: &str) {
        self.names.insert(title, name.to_string());
    }

    /// Every name of a dictionary group finds the titles any of its names finds.
    pub fn widen(&mut self, groups: impl Iterator<Item = Vec<String>>) {
        for group in groups {
            let keys: Vec<String> = group.iter().map(|n| dictionary::norm(n)).filter(|k| k.chars().count() >= 3).collect();
            let titles: Vec<(u64, bool)> = keys.iter().filter_map(|k| self.by_key.get(k)).flatten().map(|(t, _)| (*t, false)).collect();
            if titles.is_empty() {
                continue;
            }
            for k in keys {
                let at = self.by_key.entry(k).or_default();
                for t in &titles {
                    if !at.iter().any(|(x, _)| *x == t.0) {
                        at.push(*t);
                    }
                }
            }
        }
    }

    /// The title a name means, as (tag name, titles): None when nothing fits, Err when the
    /// name fits two different games.
    pub fn resolve(&self, text: &str) -> Option<std::result::Result<(String, Vec<u64>), ()>> {
        let tokens = strip_noise(dictionary::tokenize(text));
        if tokens.is_empty() {
            return None;
        }
        let full = key(&tokens);
        let full_len = full.chars().count();
        let mut candidates: Vec<String> = vec![full.clone()];
        for len in (1..tokens.len()).rev() {
            candidates.push(key(&tokens[..len]));
        }
        if tokens[0].cjk {
            let chars: Vec<char> = tokens[0].norm.chars().collect();
            for n in (3..chars.len()).rev() {
                candidates.push(chars[..n].iter().collect());
            }
        }
        for k in candidates {
            let len = k.chars().count();
            if len < 4 && !(len >= 2 && k.chars().any(dictionary::is_cjk) && k == full) {
                continue;
            }
            if len * 2 < full_len {
                break;
            }
            let Some(hits) = self.by_key.get(&k) else { continue };
            let names: HashSet<String> = hits.iter().filter_map(|(t, _)| self.names.get(t)).map(|n| dictionary::norm(n)).collect();
            let tag = hits.iter().find(|(_, own)| *own).or(hits.first()).and_then(|(t, _)| self.names.get(t)).cloned();
            let Some(tag) = tag else { continue };
            if names.len() > 1 {
                // Different games: only the ones whose own name this is, if they agree.
                let own: Vec<u64> = hits.iter().filter(|(_, own)| *own).map(|(t, _)| *t).collect();
                let own_names: HashSet<String> = own.iter().filter_map(|t| self.names.get(t)).map(|n| dictionary::norm(n)).collect();
                if own.is_empty() || own_names.len() > 1 {
                    return Some(Err(()));
                }
                return Some(Ok((tag, own)));
            }
            return Some(Ok((tag, hits.iter().map(|(t, _)| *t).collect())));
        }
        None
    }
}

pub async fn run(app: &App, progress: &Progress) -> Result<AutotagSummary> {
    let pool = &app.pool;
    let mut s = AutotagSummary::default();
    progress.force(0, None, "auto-tag: reading title names").await;
    let mut index = TitleIndex::default();
    let game = "EXISTS (SELECT 1 FROM title_releases tr JOIN `release` r ON r.id = tr.release_id AND r.deleted = FALSE AND r.material IS NULL WHERE tr.title_id = t.id)";
    for r in sqlx::query(sqlx::AssertSqlSafe(format!("SELECT t.id, t.name FROM title t WHERE t.deleted = FALSE AND {game}"))).fetch_all(pool).await? {
        let (id, name): (u64, String) = (r.try_get("id")?, r.try_get("name")?);
        index.add(id, &name, true);
        index.set_name(id, &name);
    }
    for sql in [
        format!("SELECT tl.title_id AS id, tl.name FROM title_locales tl JOIN title t ON t.id = tl.title_id AND t.deleted = FALSE WHERE {game}"),
        format!("SELECT tan.title_id AS id, tan.name FROM title_alt_names tan JOIN title t ON t.id = tan.title_id AND t.deleted = FALSE WHERE {game}"),
    ] {
        for r in sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_all(pool).await? {
            index.add(r.try_get("id")?, &r.try_get::<String, _>("name")?, false);
        }
    }
    let dict = dictionary::dict(pool).await?;
    index.widen(dict.groups());

    progress.force(0, None, "auto-tag: reading the audio library").await;
    let manual: HashSet<u64> = sqlx::query_scalar("SELECT DISTINCT t.audio_file_id FROM audio_file_tags t WHERE t.source <> 'auto'").fetch_all(pool).await?.into_iter().collect();
    // Albums by folder and album name: one decision per album.
    let mut albums: BTreeMap<(String, String), Vec<(u64, Option<String>)>> = BTreeMap::new();
    for r in sqlx::query("SELECT id, path, album, title FROM audio_files WHERE deleted = FALSE").fetch_all(pool).await? {
        let id: u64 = r.try_get("id")?;
        if manual.contains(&id) {
            s.skipped_manual += 1;
            continue;
        }
        let path: String = r.try_get("path")?;
        let album: Option<String> = r.try_get::<Option<String>, _>("album")?.filter(|a| !a.trim().is_empty());
        let folder = std::path::Path::new(&path).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
        albums.entry((folder, album.unwrap_or_default())).or_default().push((id, r.try_get("title")?));
    }
    let total = albums.len() as u64;
    let mut tagged_titles: HashSet<u64> = HashSet::new();
    let mut keyword_names: HashMap<String, String> = HashMap::new();
    for (i, ((folder, album), files)) in albums.into_iter().enumerate() {
        progress.report(i as u64, Some(total), &format!("auto-tag: album {} of {total} ({} matched)", i + 1, s.matched)).await;
        s.albums += 1;
        let mut texts: Vec<String> = Vec::new();
        if !album.is_empty() {
            texts.push(album.clone());
        }
        if let Some(f) = std::path::Path::new(&folder).file_name() {
            texts.push(f.to_string_lossy().to_string());
        }
        if files.len() == 1 {
            if let Some(t) = files[0].1.clone().filter(|t| !t.trim().is_empty()) {
                texts.push(t);
            }
        }
        // Then the same names with what the dictionary knows swapped into the other script: a
        // series name carries the number across (ホゲクエスト2 -> "Hoge Quest 2").
        let translated: Vec<String> = texts.iter().flat_map(|t| dict.variants(t, 4)).collect();
        texts.extend(translated);
        let mut decision = None;
        for text in &texts {
            match index.resolve(text) {
                Some(Ok(found)) => {
                    decision = Some(found);
                    break;
                }
                Some(Err(())) => {
                    s.ambiguous += 1;
                    break;
                }
                None => {}
            }
        }
        let ids: Vec<u64> = files.iter().map(|(id, _)| *id).collect();
        let list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM audio_file_tags WHERE source = 'auto' AND audio_file_id IN ({list})"))).execute(pool).await?;
        let Some((tag, titles)) = decision else { continue };
        s.matched += 1;
        // The keyword's own spelling, so the files and the titles carry one tag.
        let name = match keyword_names.get(&tag) {
            Some(n) => n.clone(),
            None => {
                sqlx::query("INSERT IGNORE INTO keyword (name) VALUES (?)").bind(&tag).execute(pool).await?;
                let stored: String = sqlx::query_scalar("SELECT name FROM keyword WHERE name = ? LIMIT 1").bind(&tag).fetch_optional(pool).await?.unwrap_or_else(|| tag.clone());
                keyword_names.insert(tag.clone(), stored.clone());
                stored
            }
        };
        let values = vec!["(?, ?, 'auto')"; ids.len()].join(", ");
        let mut q = sqlx::query(sqlx::AssertSqlSafe(format!("INSERT IGNORE INTO audio_file_tags (audio_file_id, name, source) VALUES {values}")));
        for id in &ids {
            q = q.bind(id).bind(&name);
        }
        s.files_tagged += q.execute(pool).await?.rows_affected();
        for t in titles {
            crate::database::providers::metadata::set_title_keywords(pool, t, "auto", &[name.as_str()]).await?;
            tagged_titles.insert(t);
        }
    }
    s.titles_tagged = tagged_titles.len() as u64;
    s.stale_title_tags = sqlx::query("DELETE tk FROM title_keywords tk JOIN keyword k ON k.id = tk.keyword_id WHERE tk.source = 'auto' AND NOT EXISTS (SELECT 1 FROM audio_file_tags aft WHERE aft.name = k.name)")
        .execute(pool)
        .await?
        .rows_affected();
    progress
        .force(total, Some(total.max(1)), &format!("done: {} of {} album(s) matched, {} file(s) tagged for {} title(s); {} ambiguous; {} file(s) with your own tags left alone", s.matched, s.albums, s.files_tagged, s.titles_tagged, s.ambiguous, s.skipped_manual))
        .await;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> TitleIndex {
        let mut i = TitleIndex::default();
        for (id, name) in [(1, "Hoge Quest"), (2, "Hoge Quest 2"), (3, "Fuga Saga: The Piyo"), (4, "Hoge Quest"), (5, "Mogera"), (6, "Mogera")] {
            i.add(id, name, true);
            i.set_name(id, name);
        }
        i.add(1, "ホゲクエスト", false);
        i.add(5, "Shared Alias", false);
        i.add(7, "Shared Alias", true);
        i.set_name(7, "Shared Alias");
        i.widen(vec![vec!["Fuga Saga".to_string(), "フガサーガ".to_string()]].into_iter());
        i
    }

    #[test]
    fn soundtrack_words_come_off_the_end_but_a_sequel_number_stays() {
        let k = |s: &str| key(&strip_noise(dictionary::tokenize(s)));
        assert_eq!(k("Hoge Quest Original Soundtrack Vol. 2"), "hogequest");
        assert_eq!(k("Hoge Quest 2 Original Soundtrack"), "hogequest2");
        assert_eq!(k("ホゲクエスト オリジナル・サウンドトラック"), "ホゲクエスト");
        assert_eq!(k("ホゲクエストサウンドトラック"), "ホゲクエスト");
    }

    #[test]
    fn albums_resolve_to_titles() {
        let i = index();
        assert_eq!(i.resolve("Hoge Quest OST"), Some(Ok(("Hoge Quest".to_string(), vec![1, 4]))), "the same game on two platforms: both");
        assert_eq!(i.resolve("Hoge Quest 2 Original Soundtrack"), Some(Ok(("Hoge Quest 2".to_string(), vec![2]))));
        assert_eq!(i.resolve("ホゲクエスト サウンドトラック").map(|r| r.map(|(n, _)| n)), Some(Ok("Hoge Quest".to_string())), "by a localized name");
        assert_eq!(i.resolve("フガサーガ").map(|r| r.map(|(n, _)| n)), Some(Ok("Fuga Saga: The Piyo".to_string())), "by the dictionary and the part before the subtitle");
        assert_eq!(i.resolve("Hoge Quest Symphonic").map(|r| r.map(|(n, _)| n)), Some(Ok("Hoge Quest".to_string())), "starts with a name covering half of it");
        assert_eq!(i.resolve("Hoge Quest Fuga Piyo Mogera Concert Live"), None, "a name covering less than half is no match");
        assert_eq!(i.resolve("Shared Alias").map(|r| r.map(|(_, t)| t)), Some(Ok(vec![7])), "an alias of another game loses to a title's own name");
        assert_eq!(i.resolve("Unknown Album"), None);
    }
}
