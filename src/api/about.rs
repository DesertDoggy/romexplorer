//! The About page: this build's version and the release notes, which ship inside the
//! binary (`docs/release_notes.md`, read at build time) so an installed copy can show the
//! notes for exactly the version it is.
//!
//! The notes are translated per `ui_language` (`docs/release_notes.<lang>.md`), and the
//! clients ask with `?lang=`. A translation that has no section for this version is behind
//! the English and is not shown: stale notes in the reader's language would misstate what
//! the build does, so the English wins until the translation catches up.

use serde_json::{json, Value};

pub const RELEASE_NOTES: &str = include_str!("../../docs/release_notes.md");

/// The translated notes, by language code.
pub const TRANSLATED_NOTES: &[(&str, &str)] = &[("ja", include_str!("../../docs/release_notes.ja.md"))];

pub fn about(lang: Option<&str>) -> Value {
    let (lang, notes) = notes_for(lang);
    json!({ "version": env!("CARGO_PKG_VERSION"), "notes_language": lang, "notes_markdown": notes, "notes_html": markdown_to_html(notes) })
}

/// The notes in `lang` when there is a translation that covers this version, else English.
fn notes_for(lang: Option<&str>) -> (&'static str, &'static str) {
    TRANSLATED_NOTES
        .iter()
        .find(|(code, notes)| Some(*code) == lang && names_this_version(notes))
        .map(|(code, notes)| (*code, *notes))
        .unwrap_or(("en", RELEASE_NOTES))
}

fn names_this_version(notes: &str) -> bool {
    notes.contains(&format!("# romexplorer {}", env!("CARGO_PKG_VERSION")))
}

/// The subset of Markdown the release notes use -- headings, paragraphs, bullet lists,
/// pipe tables, `**bold**` and `` `code` `` -- as HTML. Not a general Markdown renderer:
/// anything else comes out as the text it is, escaped.
pub fn markdown_to_html(md: &str) -> String {
    let mut out = String::with_capacity(md.len() * 2);
    let mut para: Vec<String> = Vec::new();
    let mut in_list = false;
    let mut table: Vec<Vec<String>> = Vec::new();

    fn flush_para(out: &mut String, para: &mut Vec<String>) {
        if !para.is_empty() {
            out.push_str("<p>");
            out.push_str(&para.join(" "));
            out.push_str("</p>\n");
            para.clear();
        }
    }
    fn close_list(out: &mut String, in_list: &mut bool) {
        if *in_list {
            out.push_str("</ul>\n");
            *in_list = false;
        }
    }
    fn flush_table(out: &mut String, table: &mut Vec<Vec<String>>) {
        if table.is_empty() {
            return;
        }
        out.push_str("<table class=\"grid\">");
        for (i, row) in table.iter().enumerate() {
            let tag = if i == 0 { "th" } else { "td" };
            out.push_str("<tr>");
            for cell in row {
                out.push_str(&format!("<{tag}>{cell}</{tag}>"));
            }
            out.push_str("</tr>");
        }
        out.push_str("</table>\n");
        table.clear();
    }

    for raw in md.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.starts_with('|') {
            flush_para(&mut out, &mut para);
            close_list(&mut out, &mut in_list);
            let cells: Vec<&str> = trimmed.trim_matches('|').split('|').map(str::trim).collect();
            // The header separator (`|---|---|`) carries no text.
            if cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':')) {
                continue;
            }
            table.push(cells.into_iter().map(inline).collect());
            continue;
        }
        flush_table(&mut out, &mut table);
        if trimmed.is_empty() {
            flush_para(&mut out, &mut para);
            close_list(&mut out, &mut in_list);
        } else if let Some(h) = trimmed.strip_prefix("### ") {
            flush_para(&mut out, &mut para);
            close_list(&mut out, &mut in_list);
            out.push_str(&format!("<h4>{}</h4>\n", inline(h)));
        } else if let Some(h) = trimmed.strip_prefix("## ") {
            flush_para(&mut out, &mut para);
            close_list(&mut out, &mut in_list);
            out.push_str(&format!("<h3>{}</h3>\n", inline(h)));
        } else if let Some(h) = trimmed.strip_prefix("# ") {
            flush_para(&mut out, &mut para);
            close_list(&mut out, &mut in_list);
            out.push_str(&format!("<h2>{}</h2>\n", inline(h)));
        } else if let Some(item) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            flush_para(&mut out, &mut para);
            if !in_list {
                out.push_str("<ul>\n");
                in_list = true;
            }
            out.push_str(&format!("<li>{}</li>\n", inline(item)));
        } else if in_list && line.starts_with("  ") {
            // A wrapped list item continues on an indented line.
            let end = out.trim_end_matches('\n').len();
            if out[..end].ends_with("</li>") {
                out.truncate(end - "</li>".len());
                out.push(' ');
                out.push_str(&inline(trimmed));
                out.push_str("</li>\n");
            }
        } else {
            close_list(&mut out, &mut in_list);
            para.push(inline(trimmed));
        }
    }
    flush_table(&mut out, &mut table);
    flush_para(&mut out, &mut para);
    close_list(&mut out, &mut in_list);
    out
}

/// One line of text: escaped, with `**bold**` and `` `code` `` spans.
fn inline(text: &str) -> String {
    let escaped = escape(text);
    let mut out = String::with_capacity(escaped.len() + 16);
    let mut rest = escaped.as_str();
    let mut bold = false;
    let mut code = false;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**") {
            if !code {
                out.push_str(if bold { "</b>" } else { "<b>" });
                bold = !bold;
                rest = after;
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('`') {
            out.push_str(if code { "</code>" } else { "<code>" });
            code = !code;
            rest = after;
            continue;
        }
        let next = rest.char_indices().nth(1).map(|(i, _)| i).unwrap_or(rest.len());
        out.push_str(&rest[..next]);
        rest = &rest[next..];
    }
    if code {
        out.push_str("</code>");
    }
    if bold {
        out.push_str("</b>");
    }
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_notes_shapes_render() {
        let html = markdown_to_html("# v1\n\nText with **bold** and `code` <b>.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- one\n- two\n  wrapped\n");
        assert!(html.contains("<h2>v1</h2>"));
        assert!(html.contains("<p>Text with <b>bold</b> and <code>code</code> &lt;b&gt;.</p>"), "{html}");
        assert!(html.contains("<table class=\"grid\"><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>"), "{html}");
        assert!(html.contains("<ul>\n<li>one</li>\n<li>two wrapped</li>\n</ul>"), "{html}");
    }

    #[test]
    fn the_shipped_notes_name_this_version() {
        assert!(names_this_version(RELEASE_NOTES), "docs/release_notes.md has no section for {}", env!("CARGO_PKG_VERSION"));
        for (lang, notes) in TRANSLATED_NOTES {
            assert!(names_this_version(notes), "docs/release_notes.{lang}.md has no section for {} -- translate the new notes", env!("CARGO_PKG_VERSION"));
        }
    }

    /// A translation has the English's shape: the same versions, and the same number of
    /// sections, bullets and table rows under each. Catches a release section, or a line
    /// added to one, that never got translated.
    #[test]
    fn the_translated_notes_match_the_english() {
        fn shape(md: &str) -> Vec<String> {
            md.lines()
                .filter_map(|l| {
                    if let Some(v) = l.strip_prefix("# ") {
                        Some(v.to_string())
                    } else if l.starts_with("## ") {
                        Some("##".into())
                    } else if l.starts_with("- ") {
                        Some("-".into())
                    } else if l.starts_with('|') {
                        Some("|".into())
                    } else {
                        None
                    }
                })
                .collect()
        }
        for (lang, notes) in TRANSLATED_NOTES {
            assert_eq!(shape(notes), shape(RELEASE_NOTES), "docs/release_notes.{lang}.md does not match docs/release_notes.md");
        }
    }

    #[test]
    fn the_notes_follow_the_language() {
        assert_eq!(notes_for(Some("ja")).0, "ja");
        assert_eq!(notes_for(Some("en")), ("en", RELEASE_NOTES));
        assert_eq!(notes_for(Some("xx")).0, "en");
        assert_eq!(notes_for(None).0, "en");
    }
}
