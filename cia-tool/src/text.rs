//! Text decoding helpers.
//!
//! Text files handed to the tool (file lists, aes_keys.txt) can come from any
//! editor: UTF-8 with or without a BOM, UTF-16 LE/BE (Windows Notepad "Unicode"),
//! UTF-32, or a legacy Japanese code page (Shift_JIS / CP932). SMDH titles inside
//! 3DS files are always UTF-16LE.

use encoding_rs::{Encoding, SHIFT_JIS, UTF_16BE, UTF_16LE};

/// Which encoding `decode_text` decided the bytes were in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Utf32Le,
    Utf32Be,
    ShiftJis,
}

/// Decode a text file of unknown encoding into a `String`.
pub fn decode_text(bytes: &[u8]) -> (String, TextEncoding) {
    // Byte order marks first. UTF-32 LE must be checked before UTF-16 LE.
    if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) {
        return (decode_utf32(&bytes[4..], false), TextEncoding::Utf32Le);
    }
    if bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF]) {
        return (decode_utf32(&bytes[4..], true), TextEncoding::Utf32Be);
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (
            String::from_utf8_lossy(&bytes[3..]).into_owned(),
            TextEncoding::Utf8Bom,
        );
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return (decode_with(UTF_16LE, &bytes[2..]), TextEncoding::Utf16Le);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return (decode_with(UTF_16BE, &bytes[2..]), TextEncoding::Utf16Be);
    }

    // No BOM: guess UTF-16 from the position of zero bytes. ASCII-heavy
    // UTF-16 text has a zero in every other byte.
    if bytes.len() >= 2
        && let Some(enc) = guess_utf16(bytes)
    {
        let e = if enc == TextEncoding::Utf16Le {
            UTF_16LE
        } else {
            UTF_16BE
        };
        return (decode_with(e, bytes), enc);
    }

    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_owned(), TextEncoding::Utf8),
        Err(_) => {
            // Not valid UTF-8. Japanese Windows saves "ANSI" text as CP932.
            let (s, _, had_errors) = SHIFT_JIS.decode(bytes);
            if had_errors {
                (
                    String::from_utf8_lossy(bytes).into_owned(),
                    TextEncoding::Utf8,
                )
            } else {
                (s.into_owned(), TextEncoding::ShiftJis)
            }
        }
    }
}

fn decode_with(enc: &'static Encoding, bytes: &[u8]) -> String {
    let (s, _) = enc.decode_without_bom_handling(bytes);
    s.into_owned()
}

fn decode_utf32(bytes: &[u8], big_endian: bool) -> String {
    bytes
        .chunks_exact(4)
        .map(|c| {
            let v = if big_endian {
                u32::from_be_bytes([c[0], c[1], c[2], c[3]])
            } else {
                u32::from_le_bytes([c[0], c[1], c[2], c[3]])
            };
            char::from_u32(v).unwrap_or('\u{FFFD}')
        })
        .collect()
}

fn guess_utf16(bytes: &[u8]) -> Option<TextEncoding> {
    let sample = &bytes[..bytes.len().min(4096) & !1];
    let pairs = sample.len() / 2;
    if pairs == 0 {
        return None;
    }
    let mut even_zero = 0usize;
    let mut odd_zero = 0usize;
    for p in sample.chunks_exact(2) {
        if p[0] == 0 {
            even_zero += 1;
        }
        if p[1] == 0 {
            odd_zero += 1;
        }
    }
    // Plain UTF-8 text never contains NUL bytes, so a decent share of zeros
    // on one side is a strong signal.
    let threshold = (pairs / 4).max(1);
    if odd_zero >= threshold && even_zero < odd_zero / 4 + 1 {
        Some(TextEncoding::Utf16Le)
    } else if even_zero >= threshold && odd_zero < even_zero / 4 + 1 {
        Some(TextEncoding::Utf16Be)
    } else {
        None
    }
}

/// Decode a fixed-size, NUL-padded UTF-16LE field (as used by SMDH).
pub fn utf16le_field(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Split a decoded file list into entries: one path per line, blank lines and
/// `#` comments skipped, optional surrounding quotes removed.
pub fn parse_list(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}'))
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            l.strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .unwrap_or(l)
                .to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const JP: &str = "ほげ ふしぎなどうくつ\nふが物語.cia\n";

    fn utf16(s: &str, be: bool, bom: bool) -> Vec<u8> {
        let mut v = Vec::new();
        if bom {
            v.extend_from_slice(if be { &[0xFE, 0xFF] } else { &[0xFF, 0xFE] });
        }
        for u in s.encode_utf16() {
            v.extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
        }
        v
    }

    #[test]
    fn utf8_plain_and_bom() {
        assert_eq!(
            decode_text(JP.as_bytes()),
            (JP.to_owned(), TextEncoding::Utf8)
        );
        let mut b = vec![0xEF, 0xBB, 0xBF];
        b.extend_from_slice(JP.as_bytes());
        assert_eq!(decode_text(&b), (JP.to_owned(), TextEncoding::Utf8Bom));
    }

    #[test]
    fn utf16_with_bom() {
        assert_eq!(decode_text(&utf16(JP, false, true)).0, JP);
        assert_eq!(decode_text(&utf16(JP, true, true)).0, JP);
        assert_eq!(decode_text(&utf16(JP, true, true)).1, TextEncoding::Utf16Be);
    }

    #[test]
    fn utf16_without_bom() {
        let s = "C:\\Games\\3DS\\ぴよクエスト.cia\r\nD:\\roms\\hoge.3ds\r\n";
        assert_eq!(
            decode_text(&utf16(s, false, false)),
            (s.to_owned(), TextEncoding::Utf16Le)
        );
        assert_eq!(
            decode_text(&utf16(s, true, false)),
            (s.to_owned(), TextEncoding::Utf16Be)
        );
    }

    #[test]
    fn utf32_with_bom() {
        let mut b = vec![0xFF, 0xFE, 0, 0];
        for c in JP.chars() {
            b.extend_from_slice(&(c as u32).to_le_bytes());
        }
        assert_eq!(decode_text(&b).0, JP);
    }

    #[test]
    fn shift_jis_fallback() {
        let (bytes, _, _) = SHIFT_JIS.encode("ほげカート7.3ds");
        assert_eq!(
            decode_text(&bytes),
            ("ほげカート7.3ds".to_owned(), TextEncoding::ShiftJis)
        );
    }

    #[test]
    fn list_parsing() {
        let l = parse_list("# comment\n\"C:\\a b\\ゲーム.cia\"\n\n  x.3ds  \r\n");
        assert_eq!(
            l,
            vec!["C:\\a b\\ゲーム.cia".to_owned(), "x.3ds".to_owned()]
        );
    }

    #[test]
    fn smdh_field() {
        let mut f = utf16("とびだせ どうぶつの森", false, false);
        f.resize(0x80, 0);
        assert_eq!(utf16le_field(&f), "とびだせ どうぶつの森");
    }
}
