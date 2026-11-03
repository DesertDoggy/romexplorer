//! The UI's own text, per language: `assets/i18n/<lang>.json`, one flat object of
//! key -> string. Both apps fetch the file for the `ui_language` setting and fall back
//! to their built-in English for any key it lacks, so a language file is never required to
//! be complete. The languages this build ships are compiled in as well, so a package
//! whose `assets/i18n/` was deleted still speaks. A file on disk is laid over the compiled
//! copy, string by string: what it has wins -- which is how a translation is corrected
//! without a rebuild -- and what it lacks comes from the build. It used to replace the
//! compiled copy whole, so an install that kept its `ja.json` across an upgrade showed every
//! string added since in English.
//!
//! Platform names are here too (`platform.<name>`), but are not picked by `ui_language`:
//! the vocabulary hands them out as `PlatformInfo::name_ja` and the UIs follow the separate
//! `platform_name_language` setting. Keyed text the code has no English for (the tab
//! explanations, `tabinfo.*`) is in `en.json` as well, which is why both apps load the file
//! for English too.

use serde_json::Value;

use super::{App, Error, Result};

const BUILT_IN: &[(&str, &str)] = &[
    ("en", include_str!("../../app_root_ex/assets/i18n/en.json")),
    ("ja", include_str!("../../app_root_ex/assets/i18n/ja.json")),
    ("de", include_str!("../../app_root_ex/assets/i18n/de.json")),
    ("es", include_str!("../../app_root_ex/assets/i18n/es.json")),
    ("fr", include_str!("../../app_root_ex/assets/i18n/fr.json")),
    ("it", include_str!("../../app_root_ex/assets/i18n/it.json")),
    ("ko", include_str!("../../app_root_ex/assets/i18n/ko.json")),
    ("pl", include_str!("../../app_root_ex/assets/i18n/pl.json")),
    ("pt", include_str!("../../app_root_ex/assets/i18n/pt.json")),
];

/// The languages a file or the build provides, for a settings pulldown.
pub fn languages(app: &App) -> Vec<String> {
    let mut out: Vec<String> = BUILT_IN.iter().map(|(l, _)| l.to_string()).collect();
    if let Ok(dir) = std::fs::read_dir(app.paths.i18n_dir()) {
        for entry in dir.flatten() {
            if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
                if entry.path().extension().is_some_and(|e| e == "json") && is_code(stem) && !out.iter().any(|l| l == stem) {
                    out.push(stem.to_string());
                }
            }
        }
    }
    out.sort();
    out
}

pub fn strings(app: &App, lang: &str) -> Result<Value> {
    if !is_code(lang) {
        return Err(Error::invalid("a language is a two-letter code (en, ja)"));
    }
    let path = app.paths.i18n_dir().join(format!("{lang}.json"));
    let built_in = BUILT_IN.iter().find(|(l, _)| *l == lang).map(|(_, t)| *t);
    let on_disk = match std::fs::read_to_string(&path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(Error::internal(format!("cannot read {}: {e}", path.display()))),
    };
    if built_in.is_none() && on_disk.is_none() {
        return Err(Error::not_found(format!("no language file for {lang}")));
    }
    overlay(built_in, on_disk.as_deref()).map_err(|e| Error::internal(format!("{}: {e}", path.display())))
}

/// The build's strings with the file's laid over them. Either may be absent (a language
/// only a file provides; a package with no `assets/i18n/`).
fn overlay(built_in: Option<&str>, on_disk: Option<&str>) -> std::result::Result<Value, String> {
    let object = |text: &str| -> std::result::Result<serde_json::Map<String, Value>, String> {
        match serde_json::from_str(text).map_err(|e| e.to_string())? {
            Value::Object(map) => Ok(map),
            _ => Err("not an object of key -> text".to_string()),
        }
    };
    // The compiled copy is checked by this module's tests; only the file can be malformed.
    let mut strings = built_in.and_then(|text| object(text).ok()).unwrap_or_default();
    if let Some(text) = on_disk {
        strings.extend(object(text)?);
    }
    Ok(Value::Object(strings))
}

fn is_code(lang: &str) -> bool {
    lang.len() == 2 && lang.chars().all(|c| c.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file kept from an older build corrects what it has and hides nothing added since.
    #[test]
    fn a_file_on_disk_overrides_string_by_string() {
        let built_in = r#"{"sub.a": "A", "sub.b": "B"}"#;
        assert_eq!(overlay(Some(built_in), Some(r#"{"sub.a": "A, corrected", "extra": "X"}"#)).unwrap(), serde_json::json!({"sub.a": "A, corrected", "sub.b": "B", "extra": "X"}));
        assert_eq!(overlay(Some(built_in), None).unwrap(), serde_json::json!({"sub.a": "A", "sub.b": "B"}));
        assert_eq!(overlay(None, Some(r#"{"only": "file"}"#)).unwrap(), serde_json::json!({"only": "file"}));
        assert!(overlay(Some(built_in), Some("[]")).is_err(), "a file that is not key -> text is said, not ignored");
        assert!(overlay(Some(built_in), Some("{")).is_err());
    }

    /// A dotted key in en and not in ja is a label that will silently stay English.
    #[test]
    fn the_shipped_languages_cover_the_same_keys() {
        let keys = |text: &str| -> Vec<String> {
            let v: Value = serde_json::from_str(text).unwrap();
            let mut k: Vec<String> = v.as_object().unwrap().keys().filter(|k| *k != "_").cloned().collect();
            k.sort();
            k
        };
        let en = keys(BUILT_IN[0].1);
        // en.json carries only the dotted keys (English text is its own key elsewhere), so
        // every key it has must exist in each language; they may carry English-text keys en
        // need not.
        for (lang, text) in &BUILT_IN[1..] {
            let other = keys(text);
            let missing: Vec<&String> = en.iter().filter(|k| !other.contains(k)).collect();
            assert!(missing.is_empty(), "keys in en.json but not {lang}.json: {missing:?}");
        }
        assert!(en.contains(&"tab.about".to_string()));
    }

    /// A translation keeps the English's `{fills}` and HTML tags: a lost `{n}` shows no
    /// number, a lost or renamed tag breaks the markup around it.
    #[test]
    fn translations_keep_fills_and_tags() {
        let en: Value = serde_json::from_str(BUILT_IN[0].1).unwrap();
        let marks = |text: &str| -> Vec<String> {
            let mut out = Vec::new();
            for (open, close) in [('{', '}'), ('<', '>')] {
                let mut rest = text;
                while let Some(at) = rest.find(open) {
                    let Some(len) = rest[at..].find(close) else { break };
                    let mark = &rest[at..at + len + 1];
                    // `{"platform": ...}` in a JSON example is not a fill, nor `<data root>` a tag.
                    let inner = &mark[1..mark.len() - 1];
                    let is_mark = if open == '{' {
                        inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    } else {
                        let name = inner.trim_start_matches('/').split(' ').next().unwrap_or("");
                        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()) && (!inner.contains(' ') || inner.contains('='))
                    };
                    if is_mark {
                        out.push(mark.to_string());
                    }
                    rest = &rest[at + len + 1..];
                }
            }
            out.sort();
            out
        };
        let mut bad = Vec::new();
        for (lang, text) in &BUILT_IN[1..] {
            let v: Value = serde_json::from_str(text).unwrap();
            for (key, value) in v.as_object().unwrap() {
                let Some(value) = value.as_str() else { continue };
                if key == "_" || key.starts_with("platform.") {
                    continue;
                }
                let english = en.get(key).and_then(Value::as_str).unwrap_or_else(|| key.split_once('|').filter(|(c, _)| c.chars().all(|c| c.is_ascii_lowercase() || c == '_')).map_or(key.as_str(), |(_, t)| t));
                if marks(english) != marks(value) {
                    bad.push(format!("{lang}: {key:?} -> {value:?}"));
                }
            }
        }
        assert!(bad.is_empty(), "{} translations lost or changed a {{fill}} or tag:\n{}", bad.len(), bad.join("\n"));
    }

    /// Every platform's Japanese name is in ja.json (`platform.<name>`), the one place the
    /// translations are kept; a new platform without one would show in English.
    #[test]
    fn every_platform_has_a_japanese_name() {
        let ja: Value = serde_json::from_str(BUILT_IN[1].1).unwrap();
        let names: Vec<&str> = (0..=u16::MAX).filter_map(crate::common::models::Platform::from_igdb_id).map(|p| p.display_name()).collect();
        assert!(names.len() > 50, "only {} platforms found", names.len());
        let missing: Vec<&str> = names
            .into_iter()
            .filter(|name| ja.get(format!("platform.{name}")).and_then(Value::as_str).is_none_or(str::is_empty))
            .collect();
        assert!(missing.is_empty(), "platforms with no platform.<name> in ja.json: {missing:?}");
    }

    /// Everything both UIs pass to `tr` / `trf` / `trc` as a literal -- the explanations as
    /// much as the button labels -- has Japanese. English text is its own key, so a missing
    /// entry is not an error anywhere else: the sentence just quietly stays English.
    #[test]
    fn every_translated_literal_has_japanese() {
        let ja: Value = serde_json::from_str(BUILT_IN[1].1).unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut texts = Vec::new();
        let mut stack = vec![root.join("romexplorer-gui/src")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    literals(&std::fs::read_to_string(&path).unwrap(), &["tr(", "trf(", "trc("], &mut texts);
                }
            }
        }
        literals(&std::fs::read_to_string(root.join("src/server/console.html")).unwrap(), &["tr(", "trf(", "trc("], &mut texts);
        assert!(texts.len() > 1000, "found only {} literals -- the scanner is broken", texts.len());
        let mut missing: Vec<&String> = texts.iter().filter(|t| t.chars().any(|c| c.is_ascii_alphabetic()) && ja.get(t.as_str()).is_none()).collect();
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "{} UI strings have no Japanese in ja.json: {missing:#?}", missing.len());
    }

    /// The string literal each call in `calls` opens with (for `trc`, the text after the
    /// context). Arguments that are not a literal, and JS templates with `${...}`, are skipped.
    fn literals(src: &str, calls: &[&str], out: &mut Vec<String>) {
        let b = src.as_bytes();
        for call in calls {
            let mut from = 0;
            while let Some(at) = src[from..].find(call) {
                let start = from + at;
                from = start + call.len();
                if start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_') {
                    continue;
                }
                let mut i = from;
                while i < b.len() && b[i].is_ascii_whitespace() {
                    i += 1;
                }
                let Some((mut text, end)) = literal(src, i) else { continue };
                if *call == "trc(" {
                    let mut j = end;
                    while j < b.len() && (b[j] == b',' || b[j].is_ascii_whitespace()) {
                        j += 1;
                    }
                    match literal(src, j) {
                        Some((t, _)) => text = t,
                        None => continue,
                    }
                }
                out.push(text);
            }
        }
    }

    /// The literal at `i` (Rust "..", or JS '..' ".." `..`) and the index after it.
    fn literal(src: &str, i: usize) -> Option<(String, usize)> {
        let quote = *src.as_bytes().get(i)?;
        if !matches!(quote, b'"' | b'\'' | b'`') {
            return None;
        }
        let mut out = String::new();
        let mut chars = src[i + 1..].char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            match c {
                c if c as u32 == quote as u32 => return Some((out, i + 1 + at + 1)),
                '$' if quote == b'`' && chars.peek().is_some_and(|(_, n)| *n == '{') => return None,
                '\\' => match chars.next()?.1 {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    // A Rust line continuation: the newline and the next line's indent vanish.
                    '\n' => {
                        while chars.peek().is_some_and(|(_, n)| n.is_whitespace()) {
                            chars.next();
                        }
                    }
                    'u' => {
                        let rest = &src[i + 1 + chars.peek()?.0..];
                        let (hex, skip) = if let Some(r) = rest.strip_prefix('{') { (&r[..r.find('}')?], r.find('}')? + 2) } else { (rest.get(..4)?, 4) };
                        out.push(char::from_u32(u32::from_str_radix(hex, 16).ok()?)?);
                        for _ in 0..skip {
                            chars.next();
                        }
                    }
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
        None
    }
}
