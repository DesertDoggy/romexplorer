//! Module registry for the vendored, de-redis'd provider clients. Deliberately does NOT
//! vendor playmatch's real `providers/mod.rs`: that file is almost entirely the background
//! match-cron pipeline (`MetadataProvider` trait, `drive_match_pipeline`, `write_auto_match_*`,
//! all `sea_orm::DbConn`/`entity::` based) -- not used here at all, since this crate only
//! calls each provider's raw HTTP client directly. See ../../README.md for what's vendored
//! and why.

pub mod emuready;
pub mod hasheous;
pub mod igdb;
pub mod mobygames;
pub mod screenscraper;
pub mod steamgriddb;
pub mod thegamesdb;

/// Tuned for IGDB's 4 req/s rate limit; kept here only because a few client constructors
/// pass it to `metrics::set_provider_concurrency_configured` (a no-op in this crate -- see
/// `crate::metrics`). Matches upstream's `providers::DEFAULT_CHUNK_SIZE`.
pub const DEFAULT_CHUNK_SIZE: usize = 4;
