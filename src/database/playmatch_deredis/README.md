# playmatch_deredis

A vendored, de-redis'd subset of [playmatch](https://github.com/RetroRealm/playmatch)
(MIT-licensed) -- specifically just its raw, live third-party metadata provider clients.
Used here purely as a library of "how to talk to IGDB/ScreenScraper/etc." (auth, request
building, typed response models); this project's own hash-matching, caching, and
rate-limit tracking are entirely separate, MariaDB-backed, and built on top of these
clients, not inherited from playmatch.

## Why this exists

This project already does its own hash-based ROM identification against self-downloaded
DAT files (see `src/database/`), so playmatch's own matching/hash-lookup logic would be
redundant. What playmatch actually has that's worth reusing is its metadata provider
clients -- IGDB's ~3000 lines of typed models and Apicalypse query builder alone would be
real work to reimplement from scratch. But playmatch's own server needs PostgreSQL and a
Redis-compatible store, and the Redis half has real cross-platform problems for this
project: neither real Redis nor DragonflyDB has a native Windows build, and DragonflyDB
specifically can't be ported to Windows or macOS at all, since its I/O engine is built
directly on Linux's `io_uring`/`epoll` kernel interfaces. Rather than run playmatch's server
(or its Postgres/Redis dependencies) at all, this crate lifts just the provider client code
and drops everything else.

## What's included and what's not

| Provider | Included? | Why |
|---|---|---|
| IGDB, ScreenScraper, MobyGames, SteamGridDB, EmuReady, Hasheous, TheGamesDB | Yes | Genuine live per-item API clients (search-by-name, get-by-id, etc.) |
| LaunchBox, OpenVGDB, RetroAchievements | No | Traced their actual code: their `mod.rs` client's only real operation is `ensure_imported()` -- triggering a bulk snapshot download straight into playmatch's own Postgres tables. There is no live per-item lookup to extract; the real functionality here is playmatch's own ingestion pipeline, which this project isn't using. |

For each included provider, only the raw client is vendored: `mod.rs`, `model.rs`, and any
request-builder file it has (e.g. `apicalypse.rs`/`constants.rs` for IGDB). Explicitly
excluded, confirmed via direct source tracing (not assumed): each provider's `cache.rs`
(Redis-backed response caching -- this project's own MariaDB-backed cache lives elsewhere),
`matching/` (playmatch's own DB-bridging/reconciliation logic, needs `sea-orm`/Postgres),
and `import.rs` where present (bulk ingestion).

Three small shared files are vendored plain, unchanged: `config/mod.rs` (minus a
`pub mod versions;` line for a submodule never vendored), `config/http.rs`, and
`http/abstraction.rs` (retry policy, in-flight request tracking, HTTP status
classification). `providers/mod.rs` and `metrics.rs` are **not** vendored copies of
upstream at all -- upstream's versions are almost entirely the background match-cron
pipeline (`MetadataProvider` trait, `sea-orm`/`entity`-based) and 838 lines of real
prometheus wiring, neither used here. This crate's own `providers/mod.rs` is just module
declarations, and `metrics.rs` is a deliberate no-op stand-in with the same function
signatures, so the vendored provider files don't need touching for their metrics calls to
still compile.

## Dependencies

Deliberately **zero** `sea-orm`, `sqlx`, `entity`, or `redis` in `Cargo.toml` -- confirmed
by this crate building standalone. Just the plain HTTP/serialization stack (`reqwest`,
`tokio`, `serde`, `oauth2`, `tower`, `chrono`, etc.) each provider's own request/response
code actually needs.

## What "de-redis" changed, per provider

Every included provider had a `redis_conn` field/constructor parameter that its own client
methods never actually read -- confirmed by tracing every `self.redis_conn` use site before
removing anything, not assumed from the field's presence. It existed only so playmatch's
separate `cache.rs` macro layer (`__cached_lookup_impl`/`__cached_reference_lookup_impl` in
its `cache/macros.rs`) could pull the connection off the client. Since that caching layer
isn't vendored at all, the field is dead weight -- removed from every provider's
constructor and struct.

Two providers needed more than a field removal, both handled as a "de-redis" comment
in place, not a silent behavior change:

- **ScreenScraper**: tracked per-account quota/exhaustion state (`Account`'s
  `exhausted_until_unix` etc.) in-process via `AtomicI64`, with Redis as a cross-restart/
  cross-instance persistence layer on top. `refresh_account_state_from_redis` (restoring
  that persisted state) is now a no-op; the two Redis write-back call sites
  (`mark_account_exhausted`, `persist_quota_snapshot`) are removed, since the in-process
  atomic they update first is what actually enforces the limit for the current run.
  **Known limitation**: quota tracking now resets on process restart instead of resuming
  exactly where it left off.
- **TheGamesDB**: same shape -- an in-process `AtomicI32` (`remaining_allowance`) checked
  first, with Redis as a fallback/persistence layer in `load_remaining_allowance`/
  `write_remaining_allowance`. Redis calls removed; a cache miss now goes straight to
  `probe_remaining_allowance` (one extra live API call after a restart, not a correctness
  issue).

Every removal like this is marked in the vendored source with a `De-redis:` comment
explaining exactly what upstream does there and why it's safe to drop, so a future
maintainer (or `sync_from_upstream.sh` failing loudly on a patch conflict) has the context
to re-evaluate if playmatch's upstream code around it changes.

## Additions (not just removals)

- **IGDB**: `IgdbClient::get_games_by_platform(platform_id, limit, offset)` -- one page of
  every game on a platform (`where platforms = (id); sort id asc; limit N; offset M;`),
  which upstream has no equivalent of. It's what this project's `by_platform` bulk sync
  (`database::providers::bulk::sync_igdb_platform`) pages through. Lives in the same
  `patches/01_igdb.patch` as the de-redis change, marked `romexplorer addition` in the source.

## Keeping this in sync with upstream playmatch

`scripts/sync_from_upstream.sh` copies fresh files from `external/playmatch/service/src/`
and reapplies `patches/*.patch` on top. Run it whenever that submodule is bumped to a newer
commit -- **not** as part of `cargo build`, which only ever compiles the already-patched
`.rs` files already committed under `src/`, same as any other source in this repo.

Each patch is a plain unified diff (`patches/01_igdb.patch` through
`patches/07_thegamesdb.patch`, one per provider, some covering more than one file) applied
via `patch -p1`, dry-run first. If playmatch's upstream has changed a file a patch targets,
the dry-run fails, the script reports it and stops (loud failure, by design -- a stale but
working vendored copy is never silently replaced with a broken one), and the fix is the
same regenerate-a-patch workflow already used for
`deps/mariadb-server/patches/01_client_dynamic_link.patch`: diff the new
upstream file against the old one to see what changed, re-apply the intended edit by hand,
and regenerate the patch from that.
