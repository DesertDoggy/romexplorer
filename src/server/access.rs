//! Who may call which route.
//!
//! One table, checked once in `guard` after the caller is known, rather than a check in each
//! handler -- so the whole policy is readable in one place, and a handler cannot forget it.
//!
//! **Default deny**: a route missing from the table needs an admin. On top of that, a test
//! reads every `.route(...)` in `server/mod.rs` and fails if one is not listed here at all, so
//! a new route is classified on purpose rather than landing wherever the default puts it.
//!
//! The classes:
//! - `Open` -- no sign-in (the static pages, `ping`, and signing in itself). The private-IP
//!   check still applies.
//! - `Read` -- any signed-in account: browse the catalogue and collection, sync it, fetch
//!   media from the server and have the server fetch a provider's art into its cache for
//!   them (the server is the household's art cache: a client never calls a provider while
//!   its server is up, so the server does it once for everyone -- for a URL naming a host,
//!   never an address, `api::media::provider_url_allowed`), and manage their own sign-in.
//! - `Upload` -- `POST /jobs`, let through for accounts with upload; `jobs_enqueue` then admits
//!   only a new-files scan for them (registering what they copied onto the share, never changing
//!   what is there).
//! - `Admin` -- everything that changes the server: settings, jobs, files, backups, sources,
//!   accounts, the catalogue, and anything that reaches a provider or the filesystem.

use axum::http::Method;

use crate::api::auth::Principal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Open,
    Read,
    Upload,
    Admin,
}

type Rule = (&'static str, &'static str);

const OPEN: &[Rule] = &[
    ("GET", "/"),
    ("GET", "/index.html"),
    ("GET", "/favicon.ico"),
    ("GET", "/player.js"),
    ("GET", "/api/v1/ping"),
    ("GET", "/icons/{file}"),
    ("GET", "/app"),
    ("GET", "/app/"),
    ("GET", "/app/{*path}"),
    ("POST", "/api/v1/auth/login"),
];

const READ: &[Rule] = &[
    // one's own sign-in
    ("GET", "/api/v1/auth/check"),
    ("GET", "/api/v1/auth/me"),
    ("POST", "/api/v1/auth/logout"),
    ("PUT", "/api/v1/auth/password"),
    ("GET", "/api/v1/auth/devices"),
    ("DELETE", "/api/v1/auth/devices/{id}"),
    // what the server is and holds
    ("GET", "/api/v1/server"),
    ("GET", "/api/v1/about"),
    ("GET", "/api/v1/status"),
    ("GET", "/api/v1/vocabulary"),
    ("GET", "/api/v1/i18n"),
    ("GET", "/api/v1/i18n/{lang}"),
    ("GET", "/api/v1/settings"),
    ("GET", "/api/v1/settings/{key}"),
    ("GET", "/api/v1/dat_platforms"),
    ("GET", "/api/v1/parallel/auto"),
    ("GET", "/api/v1/directories"),
    ("GET", "/api/v1/directories/resolve"),
    ("GET", "/api/v1/directories/{id}"),
    ("GET", "/api/v1/storage"),
    ("GET", "/api/v1/mode"),
    ("GET", "/api/v1/providers"),
    ("GET", "/api/v1/jobs"),
    ("GET", "/api/v1/jobs/counts"),
    ("GET", "/api/v1/jobs/{id}"),
    ("GET", "/api/v1/schedules"),
    ("GET", "/api/v1/schedules/{id}"),
    // the catalogue and collection
    ("GET", "/api/v1/catalog/platforms"),
    ("GET", "/api/v1/catalog/facets"),
    ("GET", "/api/v1/catalog/titles"),
    ("POST", "/api/v1/catalog/title_rows"),
    ("GET", "/api/v1/catalog/crossplatform"),
    ("GET", "/api/v1/catalog/crossplatform/platforms"),
    ("GET", "/api/v1/catalog/definitive"),
    ("GET", "/api/v1/catalog/definitive/{id}"),
    ("GET", "/api/v1/catalog/titles/{id}"),
    ("GET", "/api/v1/catalog/releases"),
    ("GET", "/api/v1/catalog/patches"),
    ("GET", "/api/v1/catalog/bios"),
    ("GET", "/api/v1/catalog/base-links"),
    ("GET", "/api/v1/catalog/releases/{id}"),
    ("GET", "/api/v1/catalog/games"),
    ("GET", "/api/v1/catalog/roms"),
    ("GET", "/api/v1/catalog/roms/{id}"),
    ("GET", "/api/v1/catalog/contents"),
    ("GET", "/api/v1/catalog/contents/{id}"),
    ("GET", "/api/v1/catalog/files"),
    ("GET", "/api/v1/catalog/files/{id}"),
    ("GET", "/api/v1/catalog/lookup"),
    ("GET", "/api/v1/catalog/keywords"),
    ("GET", "/api/v1/dictionary"),
    ("GET", "/api/v1/dictionary/variants"),
    ("GET", "/api/v1/catalog/titles/merges"),
    ("GET", "/api/v1/catalog/titles/release-orders"),
    ("GET", "/api/v1/catalog/titles/merge-suggestions"),
    ("GET", "/api/v1/review/merges"),
    ("GET", "/api/v1/review/aspects"),
    ("GET", "/api/v1/dats/sources"),
    ("GET", "/api/v1/dats/imports"),
    ("GET", "/api/v1/sets"),
    ("GET", "/api/v1/sets/{id}"),
    ("GET", "/api/v1/sets/export"),
    ("GET", "/api/v1/sets/{id}/releases"),
    // RetroAchievements, as cached (`?refresh` reaches RA, so `guard` holds it for admins)
    ("GET", "/api/v1/ra/status"),
    ("GET", "/api/v1/ra/avatar"),
    ("GET", "/api/v1/ra/games"),
    ("GET", "/api/v1/ra/games/{id}"),
    ("GET", "/api/v1/ra/games/{id}/badges"),
    ("GET", "/api/v1/ra/games/{id}/image/{kind}"),
    ("GET", "/api/v1/ra/badges/{name}"),
    // media the server already has, sync, and the phone's cache file
    ("GET", "/api/v1/media/{id}"),
    // the audio library: listening is reading
    ("GET", "/api/v1/audio"),
    ("GET", "/api/v1/audio/tags"),
    ("GET", "/api/v1/audio/playlist"),
    ("GET", "/api/v1/audio/facets"),
    ("GET", "/api/v1/audio/{id}/file"),
    ("GET", "/api/v1/audio/remote/{id}/file"),
    ("GET", "/api/v1/media/strip"),
    ("GET", "/api/v1/media/remote/{id}"),
    ("POST", "/api/v1/media/{id}/refetch"),
    ("POST", "/api/v1/media/fetch"),
    ("GET", "/api/v1/sync/manifest"),
    ("GET", "/api/v1/sync/export/{table}"),
    ("GET", "/api/v1/sync/deleted/{table}"),
    ("GET", "/api/v1/cache"),
    ("GET", "/api/v1/cache/{name}"),
];

const UPLOAD: &[Rule] = &[("POST", "/api/v1/jobs")];

/// Listed so the tripwire can tell "decided: admin" from "forgot". Anything not in any list is
/// admin too; this list just makes the decision visible.
const ADMIN: &[Rule] = &[
    ("GET", "/api/v1/users"),
    ("POST", "/api/v1/users"),
    ("PUT", "/api/v1/users/{id}"),
    ("DELETE", "/api/v1/users/{id}"),
    ("GET", "/api/v1/users/{id}/devices"),
    ("DELETE", "/api/v1/users/{id}/devices/{device_id}"),
    ("POST", "/api/v1/server/shutdown"),
    ("PUT", "/api/v1/server/ports"),
    ("GET", "/api/v1/server/database_cache"),
    ("PUT", "/api/v1/server/database_cache"),
    ("POST", "/api/v1/server/restart"),
    ("GET", "/api/v1/db/tables"),
    ("GET", "/api/v1/db/tables/{table}/rows"),
    ("GET", "/api/v1/db/optimize/preview"),
    ("PUT", "/api/v1/settings/{key}"),
    ("DELETE", "/api/v1/settings/{key}"),
    // the whole configuration as one file: it names every folder on the machine
    ("GET", "/api/v1/config/export"),
    ("POST", "/api/v1/config/import"),
    ("GET", "/api/v1/providers/credentials"),
    ("PUT", "/api/v1/providers/credentials"),
    ("POST", "/api/v1/directories"),
    ("PUT", "/api/v1/directories/{id}"),
    ("POST", "/api/v1/directories/{id}/move"),
    ("DELETE", "/api/v1/directories/{id}"),
    ("POST", "/api/v1/storage/media"),
    ("DELETE", "/api/v1/storage/media/{id}"),
    ("POST", "/api/v1/storage/temp"),
    ("GET", "/api/v1/storage/temp/pick"),
    ("DELETE", "/api/v1/storage/temp/{id}"),
    ("GET", "/api/v1/fs/dirs"),
    ("GET", "/api/v1/fs/picker"),
    ("POST", "/api/v1/fs/pick"),
    ("GET", "/api/v1/emulators"),
    ("POST", "/api/v1/emulators"),
    ("GET", "/api/v1/emulators/presets"),
    ("DELETE", "/api/v1/emulators/{id}"),
    ("GET", "/api/v1/launch/options"),
    ("POST", "/api/v1/launch"),
    ("POST", "/api/v1/sets"),
    ("POST", "/api/v1/sets/import"),
    ("PUT", "/api/v1/sets/{id}"),
    ("DELETE", "/api/v1/sets/{id}"),
    ("POST", "/api/v1/sets/{id}/releases"),
    ("DELETE", "/api/v1/sets/{id}/releases/{release_id}"),
    ("POST", "/api/v1/sets/{id}/remove"),
    ("POST", "/api/v1/sets/{id}/reorder"),
    ("POST", "/api/v1/ra/refresh"),
    ("POST", "/api/v1/ra/genres/fetch"),
    ("POST", "/api/v1/ra/patches/download"),
    ("POST", "/api/v1/media/manual"),
    ("POST", "/api/v1/media/upload"),
    ("POST", "/api/v1/audio/tags"),
    ("POST", "/api/v1/audio/tags/remove"),
    ("DELETE", "/api/v1/media/{id}"),
    ("GET", "/api/v1/backup"),
    ("POST", "/api/v1/backup"),
    ("POST", "/api/v1/backup/upload"),
    ("GET", "/api/v1/dats/files"),
    ("GET", "/api/v1/dats/files/{source}/{*name}"),
    ("GET", "/api/v1/dats/bundle"),
    ("POST", "/api/v1/dats/export"),
    ("POST", "/api/v1/dats/upload"),
    ("GET", "/api/v1/backup/{name}"),
    ("DELETE", "/api/v1/backup/{name}"),
    ("POST", "/api/v1/backup/{name}/restore"),
    ("POST", "/api/v1/cache"),
    ("DELETE", "/api/v1/cache/{name}"),
    ("POST", "/api/v1/cache/{name}/import"),
    ("PUT", "/api/v1/mode"),
    ("GET", "/api/v1/sources"),
    ("POST", "/api/v1/sources"),
    ("GET", "/api/v1/sources/{id}"),
    ("PUT", "/api/v1/sources/{id}"),
    ("DELETE", "/api/v1/sources/{id}"),
    ("GET", "/api/v1/sources/{id}/roots"),
    ("GET", "/api/v1/sources/{id}/mounts"),
    ("POST", "/api/v1/sources/{id}/mounts"),
    ("GET", "/api/v1/sources/{id}/check"),
    ("POST", "/api/v1/sources/{id}/sync"),
    ("POST", "/api/v1/sources/{id}/media"),
    ("POST", "/api/v1/sources/{id}/forget"),
    ("POST", "/api/v1/sources/{id}/trust"),
    ("DELETE", "/api/v1/mounts/{id}"),
    ("POST", "/api/v1/mounts/{id}/check"),
    ("POST", "/api/v1/copy"),
    ("POST", "/api/v1/copy/preview"),
    ("GET", "/api/v1/copy/defaults"),
    ("PUT", "/api/v1/copy/defaults"),
    ("GET", "/api/v1/integrity"),
    ("POST", "/api/v1/integrity"),
    ("POST", "/api/v1/sheets/preview"),
    ("POST", "/api/v1/sheets"),
    ("POST", "/api/v1/sheets/verify"),
    ("POST", "/api/v1/sheets/m3u/verify"),
    ("POST", "/api/v1/sheets/files/{id}/verify"),
    ("POST", "/api/v1/sheets/files/{id}/fix"),
    ("GET", "/api/v1/naming/folders"),
    ("PUT", "/api/v1/naming/folders"),
    ("POST", "/api/v1/naming/folders/reset"),
    ("GET", "/api/v1/media/reuse_prompt"),
    ("GET", "/api/v1/ai"),
    ("POST", "/api/v1/ai/clear"),
    ("GET", "/api/v1/ai/export"),
    ("POST", "/api/v1/ai/import"),
    ("POST", "/api/v1/ai/test/{provider}"),
    ("PUT", "/api/v1/ai/answers/{id}/pin"),
    ("POST", "/api/v1/jobs/{id}/retry"),
    ("POST", "/api/v1/jobs/{id}/run-next"),
    ("POST", "/api/v1/jobs/run-next"),
    ("POST", "/api/v1/jobs/clear"),
    ("POST", "/api/v1/jobs/{id}/cancel"),
    ("POST", "/api/v1/schedules"),
    ("PUT", "/api/v1/schedules/{id}"),
    ("DELETE", "/api/v1/schedules/{id}"),
    ("POST", "/api/v1/schedules/{id}/run"),
    ("POST", "/api/v1/providers/resolve-url"),
    ("GET", "/api/v1/providers/{provider}/search"),
    ("POST", "/api/v1/catalog/titles/{id}/refresh"),
    ("POST", "/api/v1/catalog/releases/{id}/refresh"),
    ("GET", "/api/v1/dev/counterparts/{file}"),
    ("POST", "/api/v1/catalog/titles/merge"),
    ("POST", "/api/v1/catalog/titles/dedupe"),
    ("POST", "/api/v1/catalog/titles/keywords"),
    ("POST", "/api/v1/catalog/titles/keywords/remove"),
    ("POST", "/api/v1/catalog/keywords/delete-unused"),
    ("POST", "/api/v1/catalog/keywords/remove-auto"),
    ("POST", "/api/v1/dictionary"),
    ("POST", "/api/v1/dictionary/remove"),
    ("POST", "/api/v1/dictionary/blocks/clear"),
    ("PATCH", "/api/v1/catalog/keywords/{id}"),
    ("DELETE", "/api/v1/catalog/keywords/{id}"),
    ("POST", "/api/v1/catalog/titles/release-orders/clear"),
    ("POST", "/api/v1/catalog/titles/{id}/release-order/move"),
    ("POST", "/api/v1/catalog/releases/{id}/games/move"),
    ("DELETE", "/api/v1/catalog/releases/{id}/games/order"),
    ("DELETE", "/api/v1/catalog/titles/{id}/release-order"),
    ("POST", "/api/v1/catalog/definitive/{id}/order/move"),
    ("DELETE", "/api/v1/catalog/definitive/{id}/order"),
    ("POST", "/api/v1/catalog/definitive/{id}/ask"),
    ("POST", "/api/v1/catalog/definitive/{id}/paste"),
    ("POST", "/api/v1/catalog/definitive/batch-estimate"),
    ("POST", "/api/v1/catalog/titles/{id}/basegame"),
    ("DELETE", "/api/v1/catalog/titles/{id}/basegame"),
    ("POST", "/api/v1/catalog/titles/merges/decouple"),
    ("POST", "/api/v1/catalog/titles/merges/{id}/decouple"),
    ("DELETE", "/api/v1/catalog/titles/{id}/keywords/{name}"),
    ("POST", "/api/v1/catalog/titles/merge-suggestions/bulk"),
    ("POST", "/api/v1/catalog/titles/merge-suggestions/{igdb_id}/{action}"),
    // Merge review: a choice taken or turned down, and the web asked (a provider call).
    ("POST", "/api/v1/review/merges/{key}/{action}"),
    ("POST", "/api/v1/catalog/titles/{id}/links"),
    ("POST", "/api/v1/catalog/titles/{id}/locales"),
    ("DELETE", "/api/v1/catalog/titles/{id}/locales"),
    ("DELETE", "/api/v1/catalog/titles/{id}/links/{provider}"),
    ("POST", "/api/v1/catalog/releases/{id}/links"),
    ("DELETE", "/api/v1/catalog/releases/{id}/links/{provider}"),
    ("POST", "/api/v1/catalog/releases/{id}/title"),
    ("POST", "/api/v1/catalog/releases/{id}/split"),
    ("POST", "/api/v1/catalog/releases/split"),
    ("POST", "/api/v1/catalog/releases/display"),
    ("POST", "/api/v1/catalog/releases/move-title"),
    ("DELETE", "/api/v1/catalog/releases/{id}/title"),
    ("POST", "/api/v1/catalog/releases/{id}/base"),
    ("POST", "/api/v1/catalog/releases/{id}/language"),
    ("POST", "/api/v1/catalog/base-links/bulk"),
];

/// Whether a route pattern (`/api/v1/sets/{id}`, `/app/{*path}`) matches a concrete path.
fn matches(pattern: &str, path: &str) -> bool {
    let mut want = pattern.split('/');
    let mut have = path.split('/');
    loop {
        match (want.next(), have.next()) {
            (None, None) => return true,
            (Some(w), _) if w.starts_with("{*") => return true,
            (Some(w), Some(h)) if w.starts_with('{') && w.ends_with('}') => {
                if h.is_empty() {
                    return false;
                }
            }
            (Some(w), Some(h)) if w == h => {}
            _ => return false,
        }
    }
}

/// The most specific rule for a path: a literal segment beats a `{param}`, so
/// `/catalog/titles/merges` is not read as `/catalog/titles/{id}`.
fn lookup(rules: &[Rule], method: &str, path: &str) -> Option<usize> {
    rules
        .iter()
        .filter(|(m, p)| *m == method && matches(p, path))
        .map(|(_, p)| p.split('/').filter(|s| !s.starts_with('{')).count())
        .max()
}

pub fn required(method: &Method, path: &str) -> Access {
    let method = method.as_str();
    // HEAD is a GET without the body.
    let method = if method == "HEAD" { "GET" } else { method };
    let candidates = [(Access::Open, OPEN), (Access::Read, READ), (Access::Upload, UPLOAD), (Access::Admin, ADMIN)];
    candidates
        .iter()
        .filter_map(|(access, rules)| lookup(rules, method, path).map(|score| (score, *access)))
        // On a tie the stricter class wins.
        .max_by_key(|(score, access)| (*score, rank(*access)))
        .map(|(_, access)| access)
        .unwrap_or(Access::Admin)
}

fn rank(access: Access) -> u8 {
    match access {
        Access::Open => 0,
        Access::Read => 1,
        Access::Upload => 2,
        Access::Admin => 3,
    }
}

/// Whether `principal` may make a request of this class.
pub fn allows(principal: &Principal, access: Access) -> bool {
    match access {
        Access::Open | Access::Read => true,
        Access::Upload => principal.is_admin() || principal.can_upload(),
        Access::Admin => principal.is_admin(),
    }
}

/// Where `?token=` is accepted: things a page puts in an `<img src>` or a download link, which
/// cannot carry a header. Everywhere else a token in the URL is ignored, so it doesn't end up in
/// history, logs and caches for no reason.
pub fn query_token_allowed(method: &Method, path: &str) -> bool {
    *method == Method::GET
        && ["/api/v1/media/", "/api/v1/audio/", "/api/v1/ra/avatar", "/api/v1/ra/badges/", "/api/v1/ra/games/", "/api/v1/backup/", "/api/v1/cache/", "/api/v1/dats/files/", "/api/v1/dats/bundle"].iter().any(|p| path.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, path: &str) -> Access {
        required(&Method::from_bytes(method.as_bytes()).unwrap(), path)
    }

    #[test]
    fn patterns_match_segments() {
        assert!(matches("/api/v1/sets/{id}", "/api/v1/sets/7"));
        assert!(!matches("/api/v1/sets/{id}", "/api/v1/sets/7/releases"));
        assert!(!matches("/api/v1/sets/{id}", "/api/v1/sets/"));
        assert!(matches("/app/{*path}", "/app/assets/x.js"));
        assert!(matches("/", "/"));
        assert!(!matches("/", "/x"));
    }

    #[test]
    fn representative_routes() {
        assert_eq!(req("GET", "/"), Access::Open);
        assert_eq!(req("POST", "/api/v1/auth/login"), Access::Open);
        assert_eq!(req("GET", "/api/v1/catalog/titles/5"), Access::Read);
        assert_eq!(req("GET", "/api/v1/media/42"), Access::Read);
        assert_eq!(req("HEAD", "/api/v1/media/42"), Access::Read);
        // The server as the household's art cache: a client fills it, an admin is not needed.
        assert_eq!(req("POST", "/api/v1/media/42/refetch"), Access::Read);
        assert_eq!(req("POST", "/api/v1/media/fetch"), Access::Read);
        assert_eq!(req("GET", "/api/v1/sync/export/local_files"), Access::Read);
        assert_eq!(req("POST", "/api/v1/jobs"), Access::Upload);
        assert_eq!(req("DELETE", "/api/v1/media/42"), Access::Admin);
        assert_eq!(req("PUT", "/api/v1/settings/x"), Access::Admin);
        assert_eq!(req("GET", "/api/v1/backup/b.zip"), Access::Admin);
        assert_eq!(req("GET", "/api/v1/db/tables"), Access::Admin);
        // A literal segment wins over a parameter.
        assert_eq!(req("GET", "/api/v1/catalog/titles/merges"), Access::Read);
        assert_eq!(req("POST", "/api/v1/catalog/titles/merge"), Access::Admin);
    }

    #[test]
    fn anything_unlisted_needs_an_admin() {
        assert_eq!(req("GET", "/api/v1/not-a-route"), Access::Admin);
        assert_eq!(req("POST", "/api/v1/catalog/titles"), Access::Admin, "a read route's path with another method");
        assert_eq!(req("GET", "/api/v1/sets/1/extra"), Access::Admin);
    }

    #[test]
    fn what_each_kind_of_caller_may_do() {
        use romexplorer_types::auth::{Role, User};
        let user = |role, can_upload| Principal::User {
            user: User { id: 1, username: "u".into(), role, can_upload, enabled: true, has_password: true, devices: 1, created_at: None, last_login_at: None },
            token_id: 1,
        };
        for p in [Principal::Local, Principal::Setup, user(Role::Admin, false)] {
            assert!(allows(&p, Access::Admin));
        }
        let plain = user(Role::User, false);
        assert!(allows(&plain, Access::Read) && !allows(&plain, Access::Upload) && !allows(&plain, Access::Admin));
        let uploader = user(Role::User, true);
        assert!(allows(&uploader, Access::Upload) && !allows(&uploader, Access::Admin));
    }

    #[test]
    fn a_token_in_the_url_only_where_a_header_cannot_go() {
        assert!(query_token_allowed(&Method::GET, "/api/v1/media/3"));
        assert!(query_token_allowed(&Method::GET, "/api/v1/backup/x.zip"));
        assert!(!query_token_allowed(&Method::GET, "/api/v1/settings"));
        assert!(!query_token_allowed(&Method::DELETE, "/api/v1/media/3"));
    }

    /// Every route the server declares, as `(method, path)`, read from the router source.
    fn declared_routes() -> Vec<(String, String)> {
        let source = include_str!("mod.rs");
        let mut out = Vec::new();
        for line in source.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix(".route(\"") else { continue };
            let Some((path, handlers)) = rest.split_once('"') else { continue };
            for method in ["get", "post", "put", "delete", "patch"] {
                let needle = format!("{method}(");
                let found = handlers.match_indices(&needle).any(|(i, _)| {
                    let before = handlers[..i].chars().last();
                    !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                });
                if found {
                    out.push((method.to_ascii_uppercase(), path.to_string()));
                }
            }
        }
        out
    }

    #[test]
    fn the_route_reader_sees_the_router() {
        let routes = declared_routes();
        assert!(routes.len() > 150, "found only {} routes", routes.len());
        assert!(routes.contains(&("PATCH".into(), "/api/v1/catalog/keywords/{id}".into())));
        assert!(routes.contains(&("DELETE".into(), "/api/v1/settings/{key}".into())));
    }

    /// A new route has to be classified here on purpose. Default deny keeps it safe either
    /// way; this keeps a regular user from silently losing a read they should have.
    #[test]
    fn every_route_is_classified() {
        let all: Vec<&Rule> = OPEN.iter().chain(READ).chain(UPLOAD).chain(ADMIN).collect();
        for (method, path) in declared_routes() {
            assert!(all.iter().any(|(m, p)| *m == method && *p == path), "{method} {path} is not in server/access.rs");
        }
    }

    #[test]
    fn no_rule_is_listed_twice() {
        let mut seen = std::collections::HashSet::new();
        for rule in OPEN.iter().chain(READ).chain(UPLOAD).chain(ADMIN) {
            assert!(seen.insert(rule), "{rule:?} is listed twice");
        }
    }
}
