//! Vendored, de-redis'd copy of a subset of [playmatch](https://github.com/RetroRealm/playmatch)
//! (MIT-licensed) -- specifically just the raw, live third-party metadata provider clients
//! (IGDB, ScreenScraper, MobyGames, SteamGridDB, EmuReady, Hasheous, TheGamesDB), with every
//! Redis-backed response-caching/quota-persistence layer and the Postgres-backed
//! matching/ingestion pipeline stripped out. See this crate's README.md for exactly what was
//! kept, what was dropped and why, and `patches/` + `scripts/sync_from_upstream.sh` for how
//! this copy is kept in sync with upstream playmatch.
//!
//! This crate deliberately depends on none of `sea-orm`, `sqlx`, `entity`, or `redis` --
//! caching and rate-limit persistence are this project's own concern (MariaDB-backed),
//! layered on top of these clients by the caller, not inherited from playmatch.

pub mod config;
pub mod http;
pub mod metrics;
pub mod providers;
