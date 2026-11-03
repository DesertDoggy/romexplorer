//! Provider availability (credentials present?), tier, default sync scope, the last known
//! quota, and how much of each provider is cached (`Coverage`).

use chrono::NaiveDateTime;
use serde::Serialize;
use sqlx::Row;

use crate::common::models::Platform;
use crate::database::providers::{sync_state, Provider, Tier};
use crate::database::settings::SyncScope;

use super::{App, Result};

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInfo {
    pub name: &'static str,
    pub available: bool,
    pub reason: Option<String>,
    pub tier: &'static str,
    pub default_scope: SyncScope,
    pub needs_credentials: bool,
    pub quota_remaining: Option<i32>,
    pub quota_resets_at: Option<NaiveDateTime>,
    /// When `quota_remaining` was last reported by the provider (TheGamesDB's monthly allowance).
    pub quota_checked_at: Option<NaiveDateTime>,
    /// Has a `provider_bulk_sync` job path (per platform) in the worker.
    pub bulk_sync: bool,
    /// `provider_query` accepts a name search for this provider.
    pub query_by_name: bool,
    /// What a manual request can ask of this provider (the console's request panel, and
    /// what `api::jobs::validate` enforces).
    pub capabilities: Capabilities,
    /// The platforms a per-platform sync or search can be asked for, when the provider only
    /// has some ([`bulk_platforms`]); `None` when any may be tried. The forms offer only
    /// these, and "every catalog platform" passes over the rest.
    pub bulk_platforms: Option<Vec<u16>>,
    pub coverage: Coverage,
}

/// How much of a provider is held locally. `cached` = rows in its cache table (for the
/// hash services, matched lookups); `fetched`/`remote_total` = what the provider-wide sync
/// record says (a full dump's entry count, RA's every-console total, IGDB's `games/count`),
/// so `fetched / remote_total` (or `cached / remote_total`) is the percentage when known.
/// `platforms` = one row per platform a bulk sync covered.
#[derive(Debug, Clone, Serialize)]
pub struct Coverage {
    pub cached: u64,
    pub fetched: Option<u64>,
    pub remote_total: Option<u64>,
    /// A full-database sync ran to the end.
    pub full: bool,
    pub synced_at: Option<NaiveDateTime>,
    pub platforms: Vec<PlatformCoverage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlatformCoverage {
    pub platform_id: u16,
    pub platform_name: String,
    pub entries: u64,
    pub remote_total: Option<u64>,
    /// The sync got that platform's whole list.
    pub complete: bool,
    pub synced_at: NaiveDateTime,
}

async fn cached_count(app: &App, provider: Provider) -> Result<u64> {
    let table = provider.cache_table();
    let filter = if matches!(provider, Provider::Hasheous | Provider::PlayMatch) { "deleted = FALSE AND hit = TRUE" } else { "deleted = FALSE" };
    let sql = format!("SELECT COUNT(*) AS n FROM `{table}` WHERE {filter}");
    let n: i64 = sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_one(&app.pool).await?.try_get("n")?;
    Ok(n as u64)
}

/// Which request scopes a provider's API supports, from each provider's own API docs
/// (researched 2026-09): keyword search, one platform's whole catalog, the whole database.
#[derive(Debug, Clone, Serialize)]
pub struct Capabilities {
    /// `provider_query` by name.
    pub keyword: bool,
    /// The name search is per system on this provider, so it needs a platform.
    pub keyword_needs_platform: bool,
    /// `provider_bulk_sync` for one platform (or every platform the catalog has).
    pub platform: bool,
    /// `provider_bulk_sync` with `full`: everything the provider has.
    pub full: bool,
    /// What each scope costs / why one is missing, shown next to the request controls.
    pub notes: &'static str,
}

/// EmuReady's systems, by its own key, for the platforms here that it has: 19 of the app's
/// 70 (its `general.systems` list, read 2026-10-12). It is a compatibility database for
/// current emulation handhelds, so nothing older than the Nintendo 64 and no cartridge
/// handheld before the DS is on it. A bulk sync over every catalog platform used to ask
/// for all 70 and report the other 51 as failures each time.
pub const EMUREADY_SYSTEMS: [(Platform, &str); 19] = [
    (Platform::PcWindows, "microsoft_windows"),
    (Platform::Xbox, "microsoft_xbox"),
    (Platform::Xbox360, "microsoft_xbox_360"),
    (Platform::Nintendo3ds, "nintendo_3ds"),
    (Platform::Nintendo64, "nintendo_64"),
    (Platform::NintendoDs, "nintendo_ds"),
    (Platform::GameCube, "nintendo_gamecube"),
    (Platform::NintendoSwitch, "nintendo_switch"),
    (Platform::Wii, "nintendo_wii"),
    (Platform::WiiU, "nintendo_wii_u"),
    (Platform::Dreamcast, "sega_dreamcast"),
    (Platform::SegaSaturn, "sega_saturn"),
    (Platform::PlayStation, "sony_playstation"),
    (Platform::PlayStation2, "sony_playstation_2"),
    (Platform::PlayStation3, "sony_playstation_3"),
    (Platform::PlayStation4, "sony_playstation_4"),
    (Platform::PlayStation5, "sony_playstation_5"),
    (Platform::PlayStationPortable, "sony_playstation_portable"),
    (Platform::PlayStationVita, "sony_playstation_vita"),
];

/// The platforms `provider` can be asked about one at a time, when it only has some.
pub fn bulk_platforms(provider: Provider) -> Option<Vec<u16>> {
    match provider {
        Provider::EmuReady => Some(EMUREADY_SYSTEMS.iter().map(|(p, _)| p.igdb_id()).collect()),
        _ => None,
    }
}

pub fn capabilities(provider: Provider) -> Capabilities {
    let c = |keyword, keyword_needs_platform, platform, full, notes| Capabilities { keyword, keyword_needs_platform, platform, full, notes };
    match provider {
        Provider::Hasheous => c(false, false, true, true, "Matches by hash only: Hasheous offers no name search. Platform = that platform's dump zip; full = the whole MetadataMap dump (~2.6 GB). Local ROMs whose hash is in the dump are linked to its IGDB/TheGamesDB/RetroAchievements/SteamGridDB ids, then a follow-up IGDB job fetches those games by id."),
        Provider::PlayMatch => c(false, false, false, false, "Hash identification only here (runs from hash match)."),
        Provider::Igdb => c(true, false, true, false, "Platform: pages of 500 at 4 requests/s. No full: IGDB's database dumps are for IGDB Data Partners only."),
        Provider::ScreenScraper => c(true, true, false, false, "Name search per system (max 30 results). No platform or full: the API has no listing endpoint, only per-game lookups, under a daily quota."),
        Provider::SteamGridDb => c(true, false, false, false, "Artwork service: name search only (its \"platform\" means a store like Steam, not a console)."),
        Provider::EmuReady => c(true, true, true, false, "Name search per system. Platform: pages of 50 (the whole site is ~6k games). No full-database endpoint."),
        Provider::MobyGames => c(true, false, false, false, "Name search. Paid API (hobbyist tier 1 request / 5 s); platform listing isn't implemented."),
        Provider::TheGamesDb => c(true, false, true, true, "Platform: 20 per page, each page uses the key's monthly allowance. Full: the official JSON dump in one download (~146 MB, ~121k games), no allowance used."),
        Provider::RetroAchievements => c(true, false, true, true, "Name search runs over the cached console lists (sync a platform first). Platform = one call per console with its hashes; full = every console (~70 calls)."),
    }
}

pub async fn list(app: &App) -> Result<Vec<ProviderInfo>> {
    let quota = sqlx::query("SELECT provider, remaining, resets_at, updated_at FROM provider_quota").fetch_all(&app.pool).await?;
    let states = sync_state::all(&app.pool).await?;
    let mut out = Vec::with_capacity(Provider::ALL.len());
    for provider in Provider::ALL {
        let mine: Vec<&sync_state::SyncState> = states.iter().filter(|s| s.provider == provider.name()).collect();
        let whole = mine.iter().find(|s| s.app_platform_id.is_none());
        let mut platforms: Vec<PlatformCoverage> = mine
            .iter()
            .filter_map(|s| {
                let id = s.app_platform_id?;
                Some(PlatformCoverage {
                    platform_id: id,
                    platform_name: Platform::from_igdb_id(id).map(|p| p.display_name().to_string()).unwrap_or_else(|| id.to_string()),
                    entries: s.entries,
                    remote_total: s.remote_total,
                    complete: s.complete,
                    synced_at: s.synced_at,
                })
            })
            .collect();
        platforms.sort_by(|a, b| a.platform_name.cmp(&b.platform_name));
        let coverage = Coverage {
            cached: cached_count(app, provider).await?,
            fetched: whole.filter(|s| s.entries > 0).map(|s| s.entries),
            remote_total: whole.and_then(|s| s.remote_total),
            full: whole.is_some_and(|s| s.complete),
            synced_at: whole.map(|s| s.synced_at),
            platforms,
        };
        let q = quota.iter().find(|r| r.try_get::<String, _>("provider").map(|p| p == provider.name()).unwrap_or(false));
        out.push(ProviderInfo {
            name: provider.name(),
            available: app.clients().is_available(provider),
            reason: app.clients().unavailable.iter().find(|(p, _)| *p == provider).map(|(_, why)| why.clone()),
            tier: match provider.tier() {
                Tier::Content => "content",
                Tier::Title => "title",
                Tier::Release => "release",
            },
            default_scope: provider.default_scope(),
            needs_credentials: provider.needs_credentials(),
            quota_remaining: q.and_then(|r| r.try_get("remaining").ok()),
            quota_resets_at: q.and_then(|r| r.try_get("resets_at").ok()),
            quota_checked_at: q.and_then(|r| r.try_get("updated_at").ok()),
            bulk_sync: capabilities(provider).platform,
            query_by_name: capabilities(provider).keyword,
            capabilities: capabilities(provider),
            bulk_platforms: bulk_platforms(provider),
            coverage,
        });
    }
    Ok(out)
}

/// One `api.env` entry as the UI sees it: whether it is set, never what it is.
#[derive(Debug, Clone, Serialize)]
pub struct CredentialView {
    pub provider: &'static str,
    pub key: &'static str,
    pub label: &'static str,
    pub secret: bool,
    pub required: bool,
    pub set: bool,
}

/// Every credential a provider reads, and which of them `api.env` holds a usable value for.
pub fn credentials(app: &App) -> Result<Vec<CredentialView>> {
    let creds = crate::database::ApiCredentials::load(&app.paths.api_env())?;
    Ok(crate::database::env_creds::CREDENTIAL_FIELDS
        .iter()
        .map(|f| CredentialView { provider: f.provider, key: f.key, label: f.label, secret: f.secret, required: f.required, set: creds.get(f.key).is_some() })
        .collect())
}

/// Sets (non-empty) or clears (empty) the given `api.env` keys. Only keys a provider reads
/// are accepted, so the file cannot be used to write arbitrary lines. Values are sealed with
/// this machine's master key on the way in, and the provider clients follow the file on their
/// next use (`App::clients`).
pub fn set_credentials(app: &App, values: &std::collections::BTreeMap<String, String>) -> Result<()> {
    let fields = crate::database::env_creds::CREDENTIAL_FIELDS;
    let mut updates: Vec<(&str, Option<&str>)> = Vec::with_capacity(values.len());
    for (key, value) in values {
        let field = fields.iter().find(|f| f.key.eq_ignore_ascii_case(key)).ok_or_else(|| super::Error::invalid(format!("{key} is not a credential any provider reads")))?;
        let value = value.trim();
        updates.push((field.key, if value.is_empty() { None } else { Some(value) }));
    }
    crate::database::env_creds::update_file(&app.paths.api_env(), &app.vault, &updates)?;
    Ok(())
}
