//! Provider credentials, read from `app_root_ex/assets/api.env` and nowhere else.
//!
//! The file is plain `KEY=VALUE` lines (`#` comments, blank lines and optional single/double
//! quotes around the value are tolerated). Values are held in memory only for as long as a
//! client needs them; nothing here ever logs, prints or persists a value, and `Debug` on
//! [`ApiCredentials`] deliberately shows key names only.
//!
//! Keys this project reads (all optional -- a missing/placeholder key just means that
//! provider's client isn't built):
//!
//! | key | provider |
//! |---|---|
//! | `igdb_id`, `igdb_key` | IGDB (Twitch client id / secret) |
//! | `screenscraper_dev_id`, `screenscraper_dev_password` | ScreenScraper developer creds (required by its API) |
//! | `screenscraper_username`, `screenscraper_pass` | ScreenScraper user account (raises quota) |
//! | `steamgrid_api_key` | SteamGridDB |
//! | `thegamesdb_api_key` (`thegamesdb_app_name` is informational) | TheGamesDB |
//! | `mob_api_key` | MobyGames |
//! | `ra_username`, `ra_api_key` | RetroAchievements web API |
//!
//! Hasheous and EmuReady need no credentials.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use super::error::DatabaseError;
use super::vault::Vault;

#[derive(Clone, Default)]
pub struct ApiCredentials {
    values: HashMap<String, String>,
}

impl fmt::Debug for ApiCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut keys: Vec<&String> = self.values.keys().collect();
        keys.sort();
        f.debug_struct("ApiCredentials").field("keys", &keys).finish()
    }
}

impl ApiCredentials {
    /// Loads `path`. A missing file is not an error (no providers configured), an unreadable
    /// one is.
    pub fn load(path: &Path) -> Result<Self, DatabaseError> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(|err| DatabaseError::Config(format!("cannot read {}: {err}", path.display())))?;
        Ok(Self::parse(&text))
    }

    pub fn parse(text: &str) -> Self {
        Self { values: super::env_file::parse(text) }
    }

    /// The value for `key`, unless absent, empty, or an obvious placeholder (`xxxx`,
    /// `your_key`, `<...>`, `changeme`, `todo`).
    pub fn get(&self, key: &str) -> Option<&str> {
        let value = self.values.get(&key.to_ascii_lowercase())?.as_str();
        if is_placeholder(value) {
            return None;
        }
        Some(value)
    }

    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Opens every `enc:v1:` value with `vault`, in place. A value that won't open is
    /// *removed* rather than kept -- passed on, the sealed text would reach a provider as if it
    /// were the key -- and its key name (never the value) is returned so the caller can say
    /// which provider is now unavailable and why.
    pub fn decrypt(&mut self, vault: &Vault) -> Vec<String> {
        let mut failed = Vec::new();
        for (key, value) in self.values.iter_mut() {
            if !Vault::is_sealed(value) {
                continue;
            }
            match vault.open(&context(key), value) {
                Ok(plain) => *value = plain,
                Err(_) => failed.push(key.clone()),
            }
        }
        for key in &failed {
            self.values.remove(key);
        }
        failed.sort();
        failed
    }

    /// Every key present in the file, whether usable or placeholder -- for diagnostics only.
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.values.keys().cloned().collect();
        keys.sort();
        keys
    }
}

/// The associated data a value is sealed with, so a sealed value only opens under its own key.
fn context(key: &str) -> String {
    format!("api.env:{}", key.to_ascii_lowercase())
}

/// `text` with every real, not-yet-sealed value sealed, and how many were.
///
/// Line for line: comments, blank lines, order, key spelling and an `export ` prefix all
/// survive, so the file is still the one a person wrote. Placeholders stay as they are -- they
/// are not secrets, and sealing one would hide that it still needs filling in.
pub fn seal_text(text: &str, vault: &Vault) -> (String, usize) {
    let mut out = String::with_capacity(text.len() * 2);
    let mut sealed = 0;
    for raw in text.split_inclusive('\n') {
        let (line, ending) = match raw.strip_suffix("\r\n") {
            Some(l) => (l, "\r\n"),
            None => match raw.strip_suffix('\n') {
                Some(l) => (l, "\n"),
                None => (raw, ""),
            },
        };
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        let (export, body) = match trimmed.strip_prefix("export ") {
            Some(rest) => ("export ", rest),
            None => ("", trimmed),
        };
        let replacement = if trimmed.is_empty() || trimmed.starts_with('#') {
            None
        } else if let Some((key, value)) = body.split_once('=') {
            let parsed = super::env_file::parse(&format!("{}={}", key.trim(), value));
            let plain = parsed.values().next().cloned().unwrap_or_default();
            if key.trim().is_empty() || is_placeholder(&plain) || Vault::is_sealed(&plain) {
                None
            } else {
                sealed += 1;
                Some(format!("{indent}{export}{}={}", key.trim_end(), vault.seal(&context(key.trim()), &plain)))
            }
        } else {
            None
        };
        out.push_str(replacement.as_deref().unwrap_or(line));
        out.push_str(ending);
    }
    (out, sealed)
}

/// Seals `path` in place (`romexplorer_server encrypt-credentials`). Written to a temporary
/// file created owner-only and renamed over the original, so an interrupted run leaves either
/// the old file or the new one, never half of each. Returns how many values were sealed.
pub fn seal_file(path: &Path, vault: &Vault) -> Result<usize, DatabaseError> {
    let text = std::fs::read_to_string(path).map_err(|e| DatabaseError::Config(format!("cannot read {}: {e}", path.display())))?;
    let (out, sealed) = seal_text(&text, vault);
    if sealed == 0 {
        return Ok(0);
    }
    replace_private(path, &out)?;
    Ok(sealed)
}

/// Writes `text` to `path` through a temporary file created owner-only and renamed over it,
/// so an interrupted write leaves either the old file or the new one, never half of each.
fn replace_private(path: &Path, text: &str) -> Result<(), DatabaseError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| DatabaseError::Config(format!("cannot create {}: {e}", parent.display())))?;
    }
    let tmp = path.with_extension("env.writing");
    let _ = std::fs::remove_file(&tmp);
    {
        use std::io::Write;
        let mut file = super::vault::create_private(&tmp).map_err(|e| DatabaseError::Config(format!("cannot create {}: {e}", tmp.display())))?;
        file.write_all(text.as_bytes()).and_then(|_| file.sync_all()).map_err(|e| DatabaseError::Config(format!("cannot write {}: {e}", tmp.display())))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| DatabaseError::Config(format!("cannot replace {}: {e}", path.display())))?;
    Ok(())
}

/// One credential a provider reads from `api.env`, as the UI asks for it.
#[derive(Debug, Clone, Copy)]
pub struct CredentialField {
    /// `Provider::name()`.
    pub provider: &'static str,
    /// The `api.env` key.
    pub key: &'static str,
    pub label: &'static str,
    /// Shown as a password field. Every value is sealed on disk either way.
    pub secret: bool,
    /// Without it the provider is unavailable; an optional one only adds something (quota).
    pub required: bool,
}

/// Every key [`super::providers::ProviderClients::from_credentials`] reads -- the module doc's
/// table, as data. Hasheous, EmuReady and PlayMatch take none.
pub const CREDENTIAL_FIELDS: &[CredentialField] = &[
    CredentialField { provider: "igdb", key: "igdb_id", label: "Twitch client ID", secret: false, required: true },
    CredentialField { provider: "igdb", key: "igdb_key", label: "Twitch client secret", secret: true, required: true },
    CredentialField { provider: "screenscraper", key: "screenscraper_dev_id", label: "developer id", secret: false, required: true },
    CredentialField { provider: "screenscraper", key: "screenscraper_dev_password", label: "developer password", secret: true, required: true },
    CredentialField { provider: "screenscraper", key: "screenscraper_username", label: "user name (raises the quota)", secret: false, required: false },
    CredentialField { provider: "screenscraper", key: "screenscraper_pass", label: "user password", secret: true, required: false },
    CredentialField { provider: "steamgriddb", key: "steamgrid_api_key", label: "API key", secret: true, required: true },
    CredentialField { provider: "mobygames", key: "mob_api_key", label: "API key", secret: true, required: true },
    CredentialField { provider: "thegamesdb", key: "thegamesdb_api_key", label: "API key", secret: true, required: true },
    CredentialField { provider: "retroachievements", key: "ra_username", label: "user name", secret: false, required: true },
    CredentialField { provider: "retroachievements", key: "ra_api_key", label: "web API key", secret: true, required: true },
    // Not metadata providers: the AI that answers in the app (`api::ai`, Config › AI) --
    // Edit › Merge review asks it what an International or a Director's Cut changed.
    CredentialField { provider: "gemini", key: "gemini_api_key", label: "API key (Google AI Studio)", secret: true, required: true },
    CredentialField { provider: "claude", key: "anthropic_api_key", label: "API key (Claude Console)", secret: true, required: true },
    CredentialField { provider: "openai", key: "openai_api_key", label: "API key (OpenAI Platform)", secret: true, required: true },
    CredentialField { provider: "perplexity", key: "perplexity_api_key", label: "API key (Perplexity)", secret: true, required: true },
];

/// `text` with each `(key, value)` applied: `Some` sets the key (sealed with `vault`),
/// `None` removes it. Line for line like [`seal_text`]: an existing line is rewritten where
/// it stands, comments and every other key survive, and a new key is appended.
pub fn update_text(text: &str, vault: &Vault, updates: &[(&str, Option<&str>)]) -> String {
    let wanted = |line: &str| -> Option<usize> {
        let body = line.trim_start();
        let body = body.strip_prefix("export ").unwrap_or(body);
        if body.starts_with('#') {
            return None;
        }
        let (key, _) = body.split_once('=')?;
        updates.iter().position(|(k, _)| k.eq_ignore_ascii_case(key.trim()))
    };
    let mut done = vec![false; updates.len()];
    let mut out = String::with_capacity(text.len() + 256);
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\r', '\n']);
        let ending = &raw[line.len()..];
        match wanted(line) {
            // A key set twice keeps only its first line; later duplicates are dropped so the
            // file cannot disagree with itself.
            Some(i) if done[i] => {}
            Some(i) => {
                done[i] = true;
                if let (key, Some(value)) = updates[i] {
                    out.push_str(&format!("{key}={}{ending}", vault.seal(&context(key), value)));
                }
            }
            None => out.push_str(raw),
        }
    }
    for (i, (key, value)) in updates.iter().enumerate() {
        if let (false, Some(value)) = (done[i], value) {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&format!("{key}={}\n", vault.seal(&context(key), value)));
        }
    }
    out
}

/// [`update_text`] applied to `path` (created if missing), written owner-only and atomically.
pub fn update_file(path: &Path, vault: &Vault, updates: &[(&str, Option<&str>)]) -> Result<(), DatabaseError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(DatabaseError::Config(format!("cannot read {}: {e}", path.display()))),
    };
    replace_private(path, &update_text(&text, vault, updates))
}

fn is_placeholder(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return true;
    }
    let lower = v.to_ascii_lowercase();
    if lower.starts_with('<') && lower.ends_with('>') {
        return true;
    }
    if lower.chars().all(|c| c == 'x' || c == '*' || c == '.') {
        return true;
    }
    if matches!(lower.as_str(), "changeme" | "todo" | "none" | "null" | "placeholder") || lower.starts_with("your_") || lower.starts_with("your-") {
        return true;
    }
    // Real keys are random tokens; a value that spells out what it's supposed to be
    // ("your_api_key_here", "insert-token", "example") is a template value, not a key.
    ["your", "placeholder", "example", "api_key", "apikey", "api-key", "insert", "_here", "changeme", "secret_here", "token_here"].iter().any(|w| lower.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealing_keeps_the_file_a_person_wrote() {
        let vault = Vault::from_key([7; 32]);
        let text = "# IGDB\r\nigdb_id = abc123\r\n\nexport IGDB_KEY=\"quoted value\"\nmob_api_key=xxxx\nra_username=someone";
        let (out, sealed) = seal_text(text, &vault);
        assert_eq!(sealed, 3, "igdb_id, IGDB_KEY and ra_username; the placeholder stays");
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines[0], "# IGDB\r", "comments and CRLF endings survive");
        assert!(lines[1].starts_with("igdb_id =enc:v1:") || lines[1].starts_with("igdb_id=enc:v1:"), "{}", lines[1]);
        assert_eq!(lines[2], "");
        assert!(lines[3].starts_with("export IGDB_KEY=enc:v1:"), "{}", lines[3]);
        assert_eq!(lines[4], "mob_api_key=xxxx");
        assert!(!out.contains("abc123") && !out.contains("quoted value"));

        // Reading it back gives the same values as before, and a second pass seals nothing.
        let mut creds = ApiCredentials::parse(&out);
        assert!(creds.decrypt(&vault).is_empty());
        assert_eq!(creds.get("igdb_id"), Some("abc123"));
        assert_eq!(creds.get("igdb_key"), Some("quoted value"));
        assert_eq!(creds.get("ra_username"), Some("someone"));
        assert_eq!(seal_text(&out, &vault).1, 0);
    }

    /// A value that won't open must never reach a provider as if it were the key.
    #[test]
    fn a_value_that_wont_open_is_dropped_by_name() {
        let other = Vault::from_key([9; 32]);
        let (out, _) = seal_text("igdb_key=abc", &other);
        let mut creds = ApiCredentials::parse(&format!("{out}\nmob_api_key=plain"));
        assert_eq!(creds.decrypt(&Vault::from_key([7; 32])), vec!["igdb_key".to_string()]);
        assert_eq!(creds.get("igdb_key"), None);
        assert_eq!(creds.get("mob_api_key"), Some("plain"));
    }

    #[test]
    fn a_sealed_value_only_opens_under_its_own_key_name() {
        let vault = Vault::from_key([7; 32]);
        let (out, _) = seal_text("igdb_key=abc", &vault);
        let moved = out.replace("igdb_key", "mob_api_key");
        let mut creds = ApiCredentials::parse(&moved);
        assert_eq!(creds.decrypt(&vault), vec!["mob_api_key".to_string()]);
    }

    #[test]
    fn updating_rewrites_in_place_appends_new_keys_and_removes_cleared_ones() {
        let vault = Vault::from_key([7; 32]);
        let text = "# mine\r\nIGDB_ID=old\r\nmob_api_key=gone\r\nkeep_me=1\r\nigdb_id=dup\r\n";
        let out = update_text(text, &vault, &[("igdb_id", Some("new id")), ("mob_api_key", None), ("ra_api_key", Some("k"))]);
        let lines: Vec<&str> = out.split("\r\n").collect();
        assert_eq!(lines[0], "# mine");
        assert!(lines[1].starts_with("igdb_id=enc:v1:"), "rewritten where it stood: {}", lines[1]);
        assert_eq!(lines[2], "keep_me=1", "the cleared key is gone and the rest survives");
        assert!(!out.contains("dup") && !out.contains("old") && !out.contains("new id"));
        let mut creds = ApiCredentials::parse(&out);
        assert!(creds.decrypt(&vault).is_empty());
        assert_eq!(creds.get("igdb_id"), Some("new id"));
        assert_eq!(creds.get("ra_api_key"), Some("k"), "a new key is appended");
        assert_eq!(creds.get("mob_api_key"), None);
    }

    #[test]
    fn parses_key_values_and_ignores_placeholders() {
        let creds = ApiCredentials::parse("# comment\nfoo_key = abc123\nbar_key=\"quoted value\"\nbaz_key=xxxx\nqux_key=<your key>\ntpl_key=Your_Api_Key_Here\n\nexport zzz=1\n");
        assert_eq!(creds.get("FOO_KEY"), Some("abc123"));
        assert_eq!(creds.get("bar_key"), Some("quoted value"));
        assert_eq!(creds.get("baz_key"), None);
        assert_eq!(creds.get("qux_key"), None);
        assert_eq!(creds.get("tpl_key"), None);
        assert_eq!(creds.get("zzz"), Some("1"));
        assert!(!creds.has("missing"));
    }

    #[test]
    fn debug_never_shows_values() {
        let creds = ApiCredentials::parse("secret_key=hunter2\n");
        let shown = format!("{creds:?}");
        assert!(shown.contains("secret_key"));
        assert!(!shown.contains("hunter2"));
    }
}
