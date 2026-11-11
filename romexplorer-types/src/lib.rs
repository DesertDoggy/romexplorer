//! The API types `romexplorer` (the server) and `romexplorer-gui` share.
//!
//! Plain serde structs and enums: what a route takes and what it returns, with the same
//! module split as `romexplorer::api`. Nothing here touches a database, a socket or a file --
//! the backend crate cannot compile to wasm32 (sqlx, libloading, a C build.rs), and the GUI's
//! web build is wasm32, so the types both sides agree on live in this leaf instead.
//!
//! The backend re-exports each of these from the module it used to define it
//! (`romexplorer::api::sources::RemoteSource` is `romexplorer_types::sources::RemoteSource`),
//! so its own code and call sites are unchanged. The GUI deserialises straight into them
//! (`api::get_as::<T>`) rather than walking a `serde_json::Value`, which is both the point
//! and, as it happens, faster: no tree, no `String` per key, no clone per read.
//!
//! Two things are deliberately *not* here: `Platform` (an 82-variant enum with a backend's
//! worth of behaviour -- `PlatformSelect` carries the raw id instead and the backend
//! validates it), and anything with `&'static str` fields, which cannot deserialise.

/// The audio player script both UIs run (see the file for its interface): the web console
/// serves it as `/player.js`, the desktop app evaluates it in its webview.
pub const PLAYER_JS: &str = include_str!("player.js");

pub mod auth;
pub mod backup;
pub mod copy;
pub mod integrity;
pub mod jobs;
pub mod mobile;
#[cfg(feature = "pinning")]
pub mod pinning;
#[cfg(feature = "private-files")]
pub mod private_file;
pub mod restore;
pub mod sheets;
pub mod sources;
pub mod sqlite;
pub mod sync;
