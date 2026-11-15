//! Output file naming. Everything stays as `OsString`/`PathBuf` so names with
//! Japanese, Cyrillic, Arabic, emoji, etc. pass through untouched (the original
//! batch script stripped such characters).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Make a string safe as a file name on Windows, macOS and Linux while keeping
/// all non-ASCII characters. Only characters that are invalid in file names
/// are replaced.
pub fn sanitize_filename(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    // Collapse runs of spaces created by line breaks.
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    // Windows does not allow trailing dots or spaces.
    let trimmed = out.trim().trim_end_matches('.').trim_end().to_string();
    let upper = trimmed.to_ascii_uppercase();
    let base = upper.split('.').next().unwrap_or("");
    let reserved = matches!(base, "CON" | "PRN" | "AUX" | "NUL")
        || ((base.starts_with("COM") || base.starts_with("LPT"))
            && base.len() == 4
            && base.as_bytes()[3].is_ascii_digit());
    if trimmed.is_empty() {
        "untitled".into()
    } else if reserved {
        format!("_{trimmed}")
    } else {
        trimmed
    }
}

/// `<dir>/<stem><suffix>`, where dir is `out_dir` or the input's folder.
pub fn output_path(input: &Path, out_dir: Option<&Path>, stem: &OsStr, suffix: &str) -> PathBuf {
    let dir = match out_dir {
        Some(d) => d.to_path_buf(),
        None => input.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let mut name = OsString::from(stem);
    name.push(suffix);
    dir.join(name)
}

/// Does the file name contain "-decrypted" (output of this tool or the original script)?
pub fn looks_decrypted(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase().contains("-decrypted"))
        .unwrap_or(false)
}

/// Temporary path used while writing, renamed into place on success.
pub fn partial_path(out: &Path) -> PathBuf {
    let mut s = out.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_unicode() {
        assert_eq!(
            sanitize_filename("ほげほげ サン"),
            "ほげほげ サン"
        );
        assert_eq!(sanitize_filename("Зельда: Легенда"), "Зельда_ Легенда");
        assert_eq!(sanitize_filename("Hoge\nKart 7"), "Hoge Kart 7");
        assert_eq!(sanitize_filename("a/b\\c?.."), "a_b_c_");
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("   "), "untitled");
    }

    #[test]
    fn output_names() {
        let p = Path::new("/roms/ふが物語 3D.cia");
        let o = output_path(p, None, p.file_stem().unwrap(), " Game-decrypted.cia");
        assert_eq!(
            o,
            Path::new("/roms/ふが物語 3D Game-decrypted.cia")
        );
        assert!(looks_decrypted(&o));
        assert!(!looks_decrypted(p));
    }
}
