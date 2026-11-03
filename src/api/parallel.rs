//! The `scan_parallel_files` / `copy_parallel_files` settings: how many files a scan or a
//! copy works on at the same time, per size tier. The tiers, auto's numbers and the gate
//! that enforces them live in [`crate::rom::multiple`], shared with chdlite; this is only the
//! stored setting and how it overrides auto.
//!
//! **Auto** takes `rom::multiple::auto_limits`. **Custom** replaces any tier's number with
//! the user's. **`drive`** says what the disk is (`ssd`, `hdd`, `network`) instead of
//! detecting it, for a disk detection gets wrong; auto and custom's empty tiers follow it.

use serde::{Deserialize, Serialize};

pub use crate::rom::hash::StorageKind;
pub use crate::rom::multiple::{auto_limits, auto_limits_on, Gate, Limits, Tier, MEDIUM_MAX_BYTES, SMALL_MAX_BYTES};

/// The most files of one tier at once a custom setting may ask for.
pub const MAX_PER_TIER: u32 = 64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Auto,
    Custom,
}

/// The `scan_parallel_files` / `copy_parallel_files` settings. In custom mode a tier left
/// empty keeps its auto number, so "16 small files, the rest as before" is one field.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParallelSetting {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub large: Option<u32>,
    /// What the disk is; absent detects it (`StorageKind::for_path`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drive: Option<StorageKind>,
}

impl ParallelSetting {
    /// Rejects anything but `{mode, small, medium, large, drive}` with counts
    /// 1..=[`MAX_PER_TIER`] and a drive of `ssd`, `hdd` or `network`.
    pub fn parse(value: &serde_json::Value) -> Result<Self, String> {
        let setting: Self = serde_json::from_value(value.clone()).map_err(|e| format!("expected {{\"mode\": \"auto\"|\"custom\", \"small\"/\"medium\"/\"large\": 1-{MAX_PER_TIER}, \"drive\": \"ssd\"|\"hdd\"|\"network\"}}: {e}"))?;
        for (name, n) in [("small", setting.small), ("medium", setting.medium), ("large", setting.large)] {
            if let Some(n) = n {
                if !(1..=MAX_PER_TIER).contains(&n) {
                    return Err(format!("{name} must be 1-{MAX_PER_TIER}, not {n}"));
                }
            }
        }
        Ok(setting)
    }

    /// Auto's numbers for work reading `path`'s disk, or a disk of the kind `drive` says.
    /// Blocking: detection may run `zpool`.
    pub fn auto_limits(&self, path: &std::path::Path, flags: crate::rom::hash::HashFlags, stages: crate::rom::operation::JobStages) -> Limits {
        match self.drive {
            Some(kind) => auto_limits_on(kind, flags, stages),
            None => auto_limits(path, flags, stages),
        }
    }

    /// What applies to work reading `path`'s disk, detected off the async threads.
    pub async fn resolve_for(self, path: std::path::PathBuf, flags: crate::rom::hash::HashFlags, stages: crate::rom::operation::JobStages) -> Result<Limits, crate::api::Error> {
        let auto = tokio::task::spawn_blocking(move || self.auto_limits(&path, flags, stages)).await.map_err(|e| crate::api::Error::internal(e.to_string()))?;
        Ok(self.resolve(auto))
    }

    /// What applies, given what auto would pick.
    pub fn resolve(&self, auto: Limits) -> Limits {
        match self.mode {
            Mode::Auto => auto,
            Mode::Custom => Limits {
                small: self.small.map_or(auto.small, |n| n as usize),
                medium: self.medium.map_or(auto.medium, |n| n as usize),
                large: self.large.map_or(auto.large, |n| n as usize),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_overrides_only_the_tiers_it_names() {
        let auto = Limits { small: 8, medium: 4, large: 2 };
        let s = ParallelSetting::parse(&serde_json::json!({ "mode": "custom", "small": 16 })).unwrap();
        assert_eq!(s.resolve(auto), Limits { small: 16, medium: 4, large: 2 });
        let s = ParallelSetting::parse(&serde_json::json!({ "mode": "auto", "small": 16 })).unwrap();
        assert_eq!(s.resolve(auto), auto, "auto ignores the stored numbers");
        assert!(ParallelSetting::parse(&serde_json::json!({ "mode": "custom", "small": 0 })).is_err());
        assert!(ParallelSetting::parse(&serde_json::json!({ "mode": "custom", "large": 65 })).is_err());
        assert!(ParallelSetting::parse(&serde_json::json!({ "mode": "turbo" })).is_err());
        assert_eq!(ParallelSetting::parse(&serde_json::Value::Null).ok(), None);
    }

    #[test]
    fn drive_says_what_the_disk_is() {
        let s = ParallelSetting::parse(&serde_json::json!({ "mode": "auto", "drive": "ssd" })).unwrap();
        assert_eq!(s.drive, Some(StorageKind::Ssd));
        assert_eq!(ParallelSetting::parse(&serde_json::json!({ "mode": "auto" })).unwrap().drive, None, "absent detects");
        assert!(ParallelSetting::parse(&serde_json::json!({ "mode": "auto", "drive": "nvme" })).is_err());
        let flags = crate::rom::hash::HashFlags::CRC32;
        let stages = crate::rom::operation::JobStages::default();
        let anywhere = std::path::Path::new("/nonexistent-romexplorer-test-path");
        assert_eq!(s.auto_limits(anywhere, flags, stages), auto_limits_on(StorageKind::Ssd, flags, stages));
        let hdd = ParallelSetting { drive: Some(StorageKind::Hdd), ..s };
        assert_eq!(hdd.auto_limits(anywhere, flags, stages), auto_limits_on(StorageKind::Hdd, flags, stages));
        assert_eq!(serde_json::to_value(s).unwrap(), serde_json::json!({ "mode": "auto", "drive": "ssd" }));
    }
}
