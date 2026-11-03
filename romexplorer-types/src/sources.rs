//! Remote sources (the servers and peers a machine syncs from) and the instance mode.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteSource {
    pub id: u16,
    pub name: String,
    pub base_url: String,
    pub role: String,
    pub enabled: bool,
    /// Whether a token is stored -- never the token itself.
    pub has_token: bool,
    /// SHA-256 of the TLS certificate this machine trusts for the source, taken on the first
    /// successful connection. `None` until then (or over plain HTTP).
    #[serde(default)]
    pub tls_fingerprint: Option<String>,
    pub last_sync_at: Option<chrono::NaiveDateTime>,
    pub created_at: Option<chrono::NaiveDateTime>,
    /// Per-table sync state, so a screen can show what is current without a second call.
    pub tables: Vec<SourceTable>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceTable {
    pub table_name: String,
    pub row_count: u64,
    pub synced_at: Option<chrono::NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSpec {
    pub name: String,
    /// `http://host:port` -- an IP or a name, no trailing `/api/v1`.
    pub base_url: String,
    /// Absent on an update means "leave the stored token alone"; an empty string clears it.
    #[serde(default)]
    pub token: Option<String>,
    /// Sign in to the source with this account instead of pasting a token: the source's
    /// `auth/login` hands back a token for this machine, which is what is stored. The password
    /// itself is never kept.
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Consent to this machine's own catalogue being thrown away and replaced by the main
    /// server's. Required when designating a main server on a machine that built its own
    /// catalogue -- see `catalog_conflict` for why there is no merging option.
    #[serde(default)]
    pub replace_catalog: bool,
    /// What happens to downloaded art when that replacement runs.
    #[serde(default)]
    pub media: MediaOnReplace,
}

fn default_role() -> String {
    "peer".to_string()
}

fn default_enabled() -> bool {
    true
}

/// What to do with art whose title/release is about to be renumbered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaOnReplace {
    /// Keep the files and rebuild the links from `(source, provider_id)` after the sync
    /// (`super::sync::relink_media`). Free and instant; art downloaded before that column
    /// existed cannot be resolved and is reported.
    #[default]
    Relink,
    /// Soft-delete the rows so nothing serves art that may now be wrong. Nothing is queued
    /// here on purpose: with the rows gone `providers::media::has_media` reports the slot as
    /// empty, so the next ordinary provider query or bulk sync re-attaches the art. Forcing a
    /// bulk re-fetch would spend provider quota the user did not ask for, and on a metered
    /// connection that is not a decision to make on their behalf.
    Redownload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reachability {
    pub reachable: bool,
    pub authorized: bool,
    /// What the source offers -- a main server lists catalogue tables too, a share does not.
    pub offers_catalog: bool,
    pub detail: String,
}

/// Switching this installation between standalone and client.
///
/// Separate from the ordinary settings write so it can carry the same confirmation the main
/// server does: going `standalone -> client` means giving up a locally-built catalogue, which
/// is the identical destructive step and deserves the identical prompt. `client -> standalone`
/// takes nothing away -- it only starts permitting imports -- so it needs no confirmation.
/// What `GET /api/v1/mode` returns: enough for a screen to be honest about the choice
/// without attempting the write and parsing a refusal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeView {
    pub mode: String,
    /// Whether the setting row exists. `mode` reads `standalone` either way (the permissive
    /// default), so this is the only way to tell "chose standalone" from "was never asked" --
    /// which is what decides whether the first-run prompt appears.
    pub chosen: bool,
    pub catalog_from: Option<String>,
    pub releases: u64,
    /// Present when switching to client would replace a locally built catalogue: the same
    /// text `set_mode` refuses with, so a screen can show it *before* asking rather than after.
    pub losses: Option<String>,
    pub options: serde_json::Value,
    pub switch_warning: String,
}

/// Body of `PUT /api/v1/mode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeBody {
    pub mode: String,
    /// Consent to replacing a locally-built catalogue when switching to client mode.
    #[serde(default)]
    pub replace_catalog: bool,
    /// What happens to downloaded art when that replacement runs.
    #[serde(default)]
    pub media: MediaOnReplace,
}
