//! Raw provider responses on disk: `<provider_cache_dir>/<provider>/<key>[.<stamp>].<ext>`,
//! kept or not per the `raw_response_retention` setting.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::super::error::DatabaseError;
use super::super::settings::RawRetention;
use super::Provider;

#[derive(Debug, Clone)]
pub struct RawStore {
    root: PathBuf,
    retention: RawRetention,
}

impl RawStore {
    pub fn new(root: impl Into<PathBuf>, retention: RawRetention) -> Self {
        Self { root: root.into(), retention }
    }

    pub fn retention(&self) -> RawRetention {
        self.retention
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes `bytes` and returns the path, or `None` under `Discard`.
    pub fn save(&self, provider: Provider, key: &str, format: &str, bytes: &[u8]) -> Result<Option<PathBuf>, DatabaseError> {
        if self.retention == RawRetention::Discard {
            return Ok(None);
        }
        let dir = self.root.join(provider.name());
        std::fs::create_dir_all(&dir)?;
        let key = sanitize_key(key);
        let file_name = match self.retention {
            RawRetention::KeepAll => {
                let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                format!("{key}.{stamp}.{format}")
            }
            _ => format!("{key}.{format}"),
        };
        let path = dir.join(file_name);
        let tmp = path.with_extension(format!("{format}.part"));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(Some(path))
    }

    pub fn save_json(&self, provider: Provider, key: &str, value: &serde_json::Value) -> Result<Option<PathBuf>, DatabaseError> {
        let bytes = serde_json::to_vec_pretty(value)?;
        self.save(provider, key, "json", &bytes)
    }
}

fn sanitize_key(key: &str) -> String {
    let mut out: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect();
    if out.len() > 120 {
        out.truncate(120);
    }
    if out.is_empty() {
        out.push_str("key");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_keys() {
        assert_eq!(sanitize_key("game 12/34"), "game_12_34");
        assert_eq!(sanitize_key(""), "key");
    }
}
