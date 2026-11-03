//! Metadata providers: the seven vendored `playmatch_deredis` clients plus this project's
//! own RetroAchievements client, built from `app_root_ex/assets/api.env` credentials, with
//! MariaDB-backed caching layered on top (`cache`), raw responses on disk (`raw_store`),
//! bulk per-platform sync (`bulk`) and media download (`media`).

pub mod bulk;
pub mod cache;
pub mod igdb_localized;
pub mod change_check;
pub mod dumps;
pub mod media;
pub mod metadata;
pub mod playmatch;
pub mod ra_account;
pub mod ra_genre;
pub mod ra_hashes;
pub mod raw_store;
pub mod retroachievements;
pub mod sync_state;
pub mod thegamesdb_media;
pub mod thegamesdb_region;
pub mod xlenore;

use playmatch_deredis::providers::emuready::EmuReadyClient;
use playmatch_deredis::providers::hasheous::HasheousClient;
use playmatch_deredis::providers::igdb::IgdbClient;
use playmatch_deredis::providers::mobygames::MobyGamesClient;
use playmatch_deredis::providers::screenscraper::ScreenScraperClient;
use playmatch_deredis::providers::steamgriddb::SteamGridDbClient;
use playmatch_deredis::providers::thegamesdb::TheGamesDbClient;

use super::env_creds::ApiCredentials;
use super::settings::SyncScope;
use playmatch::PlayMatchClient;
use retroachievements::RaClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Hasheous,
    Igdb,
    ScreenScraper,
    SteamGridDb,
    EmuReady,
    MobyGames,
    TheGamesDb,
    RetroAchievements,
    /// The hosted PlayMatch service: a hash-lookup source like Hasheous.
    PlayMatch,
}

/// Which catalog tier a provider's "game" concept attaches to (docs/plan/database.md,
/// "Per-provider cache tables").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Content,
    Title,
    Release,
}

impl Provider {
    pub const ALL: [Provider; 9] = [Provider::Hasheous, Provider::PlayMatch, Provider::Igdb, Provider::ScreenScraper, Provider::SteamGridDb, Provider::EmuReady, Provider::MobyGames, Provider::TheGamesDb, Provider::RetroAchievements];

    pub fn name(self) -> &'static str {
        match self {
            Self::Hasheous => "hasheous",
            Self::Igdb => "igdb",
            Self::ScreenScraper => "screenscraper",
            Self::SteamGridDb => "steamgriddb",
            Self::EmuReady => "emuready",
            Self::MobyGames => "mobygames",
            Self::TheGamesDb => "thegamesdb",
            Self::RetroAchievements => "retroachievements",
            Self::PlayMatch => "playmatch",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let key = name.to_ascii_lowercase().replace(['-', '_', ' '], "");
        Self::ALL.into_iter().find(|p| p.name() == key)
    }

    /// Hasheous's `metadata[].source` spellings.
    pub fn from_hasheous_source(source: &str) -> Option<Self> {
        match source.to_ascii_lowercase().as_str() {
            "igdb" => Some(Self::Igdb),
            "thegamesdb" => Some(Self::TheGamesDb),
            "retroachievements" => Some(Self::RetroAchievements),
            "steamgriddb" => Some(Self::SteamGridDb),
            "screenscraper" => Some(Self::ScreenScraper),
            "mobygames" => Some(Self::MobyGames),
            _ => None,
        }
    }

    /// Implementation/build order (not runtime priority): Hasheous > IGDB > ScreenScraper >
    /// SteamGridDB > EmuReady > MobyGames > TheGamesDB, with RetroAchievements as its own track.
    pub fn implementation_order() -> [Provider; 9] {
        [Provider::Hasheous, Provider::PlayMatch, Provider::Igdb, Provider::ScreenScraper, Provider::SteamGridDb, Provider::EmuReady, Provider::MobyGames, Provider::TheGamesDb, Provider::RetroAchievements]
    }

    pub fn tier(self) -> Tier {
        match self {
            Self::Hasheous | Self::PlayMatch => Tier::Content,
            Self::Igdb | Self::MobyGames | Self::EmuReady | Self::SteamGridDb | Self::ScreenScraper => Tier::Title,
            Self::TheGamesDb | Self::RetroAchievements => Tier::Release,
        }
    }

    /// Whether this provider's art is fetched only when someone asks for it ("download art"
    /// on a selection), never on its own: TheGamesDB's monthly allowance is a few thousand
    /// calls, and automatic fetching would use it up on whatever happened to be linked first.
    pub fn media_manual_only(self) -> bool {
        matches!(self, Self::TheGamesDb)
    }

    /// The cache table for this provider's own game/lookup entity.
    pub fn cache_table(self) -> &'static str {
        match self {
            Self::Hasheous => "hasheous_lookups",
            Self::PlayMatch => "playmatch_lookups",
            Self::Igdb => "igdb_games",
            Self::ScreenScraper => "screenscraper_games",
            Self::SteamGridDb => "steamgriddb_games",
            Self::EmuReady => "emuready_games",
            Self::MobyGames => "mobygames_games",
            Self::TheGamesDb => "thegamesdb_games",
            Self::RetroAchievements => "retroachievements_games",
        }
    }

    pub fn default_scope(self) -> SyncScope {
        match self {
            Self::RetroAchievements => SyncScope::ByPlatform,
            Self::Igdb => SyncScope::List,
            _ => SyncScope::Single,
        }
    }

    pub fn needs_credentials(self) -> bool {
        !matches!(self, Self::Hasheous | Self::PlayMatch | Self::EmuReady)
    }
}

/// Every client the available credentials allow. A provider whose credentials are missing
/// (or placeholders) is simply `None` -- callers skip it, they never fail on it.
pub struct ProviderClients {
    pub hasheous: Option<HasheousClient>,
    pub igdb: Option<IgdbClient>,
    pub screenscraper: Option<ScreenScraperClient>,
    pub steamgriddb: Option<SteamGridDbClient>,
    pub emuready: Option<EmuReadyClient>,
    pub mobygames: Option<MobyGamesClient>,
    pub thegamesdb: Option<TheGamesDbClient>,
    pub retroachievements: Option<RaClient>,
    pub playmatch: Option<PlayMatchClient>,
    /// Providers that couldn't be built and why (key names only, never values).
    pub unavailable: Vec<(Provider, String)>,
}

impl ProviderClients {
    pub fn from_credentials(creds: &ApiCredentials, http: reqwest::Client) -> Self {
        let mut unavailable: Vec<(Provider, String)> = Vec::new();
        fn built<T>(unavailable: &mut Vec<(Provider, String)>, provider: Provider, result: anyhow::Result<T>) -> Option<T> {
            match result {
                Ok(client) => Some(client),
                Err(err) => {
                    unavailable.push((provider, format!("{err:#}")));
                    None
                }
            }
        }
        macro_rules! build {
            ($provider:expr, $result:expr) => {
                built(&mut unavailable, $provider, $result)
            };
        }

        let hasheous = build!(Provider::Hasheous, HasheousClient::new(http.clone()));
        let igdb = match (creds.get("igdb_id"), creds.get("igdb_key")) {
            (Some(id), Some(key)) => build!(Provider::Igdb, IgdbClient::new(id.to_string(), key.to_string(), http.clone())),
            _ => {
                unavailable.push((Provider::Igdb, "igdb_id / igdb_key not set in api.env".to_string()));
                None
            }
        };
        let screenscraper = match (creds.get("screenscraper_dev_id"), creds.get("screenscraper_dev_password")) {
            (Some(dev_id), Some(dev_pw)) => {
                let user = match (creds.get("screenscraper_username"), creds.get("screenscraper_pass")) {
                    (Some(u), Some(p)) => Some((u.to_string(), p.to_string())),
                    _ => None,
                };
                build!(Provider::ScreenScraper, ScreenScraperClient::new(dev_id.to_string(), dev_pw.to_string(), user, http.clone()))
            }
            _ => {
                unavailable.push((Provider::ScreenScraper, "screenscraper_dev_id / screenscraper_dev_password not set in api.env (developer credentials are required by the API; user credentials alone aren't enough)".to_string()));
                None
            }
        };
        let steamgriddb = match creds.get("steamgrid_api_key") {
            Some(key) => build!(Provider::SteamGridDb, SteamGridDbClient::new(key.to_string(), http.clone())),
            None => {
                unavailable.push((Provider::SteamGridDb, "steamgrid_api_key not set in api.env".to_string()));
                None
            }
        };
        let emuready = build!(Provider::EmuReady, EmuReadyClient::new(http.clone()));
        let mobygames = match creds.get("mob_api_key") {
            Some(key) => build!(Provider::MobyGames, MobyGamesClient::new(key.to_string(), http.clone())),
            None => {
                unavailable.push((Provider::MobyGames, "mob_api_key not set (or placeholder) in api.env".to_string()));
                None
            }
        };
        let thegamesdb = match creds.get("thegamesdb_api_key") {
            Some(key) => build!(Provider::TheGamesDb, TheGamesDbClient::new(http.clone(), Some(key.to_string()))),
            None => {
                unavailable.push((Provider::TheGamesDb, "thegamesdb_api_key not set in api.env".to_string()));
                None
            }
        };
        let retroachievements = match (creds.get("ra_username"), creds.get("ra_api_key")) {
            (Some(user), Some(key)) => Some(RaClient::new(http.clone(), user.to_string(), key.to_string())),
            _ => {
                unavailable.push((Provider::RetroAchievements, "ra_username / ra_api_key not set in api.env".to_string()));
                None
            }
        };

        let playmatch = Some(PlayMatchClient::new(http.clone()));

        Self { hasheous, igdb, screenscraper, steamgriddb, emuready, mobygames, thegamesdb, retroachievements, playmatch, unavailable }
    }

    pub fn is_available(&self, provider: Provider) -> bool {
        match provider {
            Provider::Hasheous => self.hasheous.is_some(),
            Provider::Igdb => self.igdb.is_some(),
            Provider::ScreenScraper => self.screenscraper.is_some(),
            Provider::SteamGridDb => self.steamgriddb.is_some(),
            Provider::EmuReady => self.emuready.is_some(),
            Provider::MobyGames => self.mobygames.is_some(),
            Provider::TheGamesDb => self.thegamesdb.is_some(),
            Provider::RetroAchievements => self.retroachievements.is_some(),
            Provider::PlayMatch => self.playmatch.is_some(),
        }
    }

    pub fn available(&self) -> Vec<Provider> {
        Provider::ALL.into_iter().filter(|p| self.is_available(*p)).collect()
    }
}

/// A `reqwest::Client` suitable for every provider (cookies for ScreenScraper, a real
/// user agent, generous timeout).
///
/// A redirect from a public host into this machine or its network (an address literal in a
/// private, loopback or link-local range, or `localhost`) is refused, so a provider URL a
/// client hands the server cannot bounce it onto the server's own network.
pub fn build_http_client() -> Result<reqwest::Client, super::error::DatabaseError> {
    let policy = reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            return attempt.error("too many redirects");
        }
        let started_inside = attempt.previous().first().is_some_and(names_internal_host);
        if names_internal_host(attempt.url()) && !started_inside {
            return attempt.error("a redirect into the local network was refused");
        }
        attempt.follow()
    });
    reqwest::Client::builder()
        .user_agent("romexplorer/0.1 (+https://github.com/DesertDoggy/romexplorer)")
        .cookie_store(true)
        .redirect(policy)
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(super::error::DatabaseError::from)
}

/// Whether `url` names `localhost` or an address that is not on the public internet.
pub fn names_internal_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else { return true };
    match host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
        Ok(ip) => is_internal_ip(ip),
        Err(_) => host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost"),
    }
}

/// An address this server must not be made to reach for someone else: loopback, private,
/// link-local, carrier-grade NAT, unspecified, broadcast, multicast, unique-local, and the
/// IPv4 forms of those inside IPv6.
pub fn is_internal_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    fn v4(ip: std::net::Ipv4Addr) -> bool {
        let o = ip.octets();
        ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() || o[0] == 0 || (o[0] == 100 && (o[1] & 0xc0) == 64)
    }
    match ip {
        IpAddr::V4(ip) => v4(ip),
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return v4(mapped);
            }
            let seg = ip.segments();
            ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || (seg[0] & 0xfe00) == 0xfc00 || (seg[0] & 0xffc0) == 0xfe80
        }
    }
}
