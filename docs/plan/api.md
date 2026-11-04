# `api` layer, `romexplorer_server` (REST), and the multi-machine model

**Status: step 1 implemented** (`src/api/`, `src/server/`, `romexplorer_server`, the
`local_*` rename) -- see "Implementation status" at the end. Steps 2-4 (worker onto `api`,
dbtool onto `api`, the Dioxus GUI) and the multi-machine pieces are still plans. Companion
to `docs/plan/gui.md` (the local desktop app) and `docs/plan/database.md` (the schema and
the worker). This file covers the layer *between* `database`/`rom` and everything a user
actually touches -- REST, GUI, CLI tools, and the worker -- plus the server/client split
that the REST side exists for.

## Context

Every user-facing consumer needs the same operations: show what's in the database, add and
configure collection directories, enqueue a scan or a DAT sync, read or change a setting,
browse the catalog. Today those are reachable only by calling `database::*` modules directly
-- `romexplorer_dbtool` and `romexplorer_worker` each hand-wire their own subset. Adding a
REST server and a GUI on the same basis would mean three or four copies of "enqueue a file
scan with a validated payload" or "list directories with their effective platform."

`api` is the one place those operations are implemented. Its consumers, in the order they'll
be built:

1. `romexplorer_server` -- axum REST + a minimal built-in browser page, first, so the
   database/jobs/config operations are visible and clickable in a browser before any other
   consumer. Exercises the full `api` surface.
2. `romexplorer_worker` -- keeps executing queued jobs; dispatches through `api` functions.
   Where an operation is both queueable and directly runnable (a DAT import from the CLI,
   a scan from a GUI "run now" button), the worker's handler and the direct path call the
   same `api` function.
3. `romexplorer_dbtool` -- moved onto `api` (a thin CLI over the same functions; behavior
   unchanged).
4. `romexplorer-gui` -- Dioxus desktop, calling `api` in-process (`docs/plan/gui.md`).
   Feature-rich, carries all web-UI operations, built last after the full `api` surface is
   proven.

```
                 romexplorer_server (REST + dev console)   <-- browser, remote client GUIs
                 romexplorer-gui   (Dioxus desktop)         in-process
                 romexplorer_dbtool (CLI)                   in-process
                 romexplorer_worker (jobs)                  in-process
                          |
                          v
                     src/api/            plain async Rust: one fn per operation, serde types
                          |
              +-----------+-----------+
              v                       v
        src/database/             src/rom/
   (owns all SQL, as today)   (scan/detect/hash/convert/archive)
```

`api` is **not** a database-access layer. The earlier decision stands: each `database`
module owns its own SQL (`lookup.rs`, `jobs.rs`, `settings.rs`, ...). `api` composes those
calls into user-level operations, validates input, and returns serde-derived types that a
REST handler serializes and a Dioxus component uses as-is. No HTTP, no GUI types, no
`axum`/`dioxus` dependency in `api` itself -- it lives in the `romexplorer` crate as
`src/api/`, next to `database` and `rom`.

## `src/api/` shape

```
api/
  mod.rs          App (shared context), Error, re-exports
  status.rs       server/database status, table counts, pending-job counts
  setup.rs        initial setup + user/password changes   [config-file driven, never routed by REST]
  directories.rs  collection roots + configured subdirectories (dir_type/platform/archival)
  storage.rs      media_dirs / temp_dirs
  settings.rs     typed get/set over database::settings keys
  jobs.rs         typed job payloads, enqueue, list, cancel/requeue, counts
  catalog.rs      read side: search, hash lookup, release/title/game detail, platform list
  dats.rs         DAT download/import (direct-run wrappers; the queued form is a job)
  providers.rs    provider availability, quota, manual query
  sync.rs         [later] remote_* mirror sync (CatalogSync), snapshot export/import
  copy/           CopyFiles (mount resolution, selection -> plan, the byte copy)
  naming.rs       per-platform destination folder names and output formats
```

- **`App`**: `{ pool: MySqlPool, paths: AppPaths, clients: Option<ProviderClients> }`,
  built once per process from `AppPaths::detect()` + `DatabaseConfig::from_env()` (the
  same construction all three existing binaries already use). Every `api` fn takes `&App`.
  This is the `Worker` struct in `romexplorer_worker.rs`, generalized.
- **`Error`**: wraps `DatabaseError` and `rom` errors with a `kind` (`NotFound`, `Invalid`,
  `Conflict`, `Unavailable`, `Internal`) so REST can map to 404/400/409/503/500 and the
  GUI can show the right message, without either inspecting SQL error strings.
- **Typed job payloads live here.** `database::jobs` stores `serde_json::Value`; `api::jobs`
  defines `FileScanRequest { directory_id, mode: FileScanMode, platform: PlatformSelect }`,
  `HashMatchRequest { directory_id, to_dat, to_hasheous, to_playmatch }`, `DatSyncRequest`,
  `ProviderBulkSyncRequest`, etc., validates them (the directory exists and isn't deleted,
  the platform id is real), and serializes to the payload shape `jobs.rs`'s doc comments
  already specify. This is the "payload types/validation" item deferred from the database
  pass, landing where it belongs -- one definition shared by REST, GUI, CLI and the worker's
  own decoding.
- **`setup.rs` is config-file driven, never a REST route.** `connection::initial_setup`,
  `set_root_password`, `create_user` change credentials. A config file (TOML or
  `.env`-style, matching the `api.env` precedent) carries `root_password`/`admin_user`/
  `admin_password`/`db_port`/`server_port`; `romexplorer_dbtool init --from-config <path>`
  (or `romexplorer_server` refusing to start until one has been applied) reads it once and
  calls `api::setup::initial_setup`. No browser-based first-run wizard, ever, deliberately
  -- a `/setup` route reachable before auth exists has an unavoidable "first on the LAN
  wins" race, and a captcha-style phrase doesn't really close that for a headless server
  (if the phrase comes from the server's own console/log, that's the same bootstrapping
  problem as a config file, just harder to script). The config-file approach sidesteps the
  question: the token is already configured before the server accepts its first
  connection, so there's no unauthenticated window at all. `romexplorer_server` simply
  never routes `setup.rs`. Same no-log/no-echo rule as `env_creds.rs`: nothing in `api`
  ever returns a password in a response type.
- **`directories.rs` needs the `database::directories` CRUD module** that was deferred from
  the database pass (insert / list / resolve-nearest-configured-ancestor). It gets written
  as part of this work, in `database`, and `api::directories` wraps it.

## `romexplorer_server`

A new binary, `src/bin/romexplorer_server.rs`, axum-based. Named `server` rather than `api`
to keep "the `api` layer" and "the REST process" from sharing a word.

- **Starts the embedded `mariadbd` if it isn't running** (the same `MariaDbServer` bring-up
  `romexplorer_dbtool serve` does), then serves. One process to start on the server machine
  besides the worker. The worker stays a separate process, as `docs/plan/database.md`'s
  Process architecture section requires -- the two never talk except through the `jobs`
  table.
- **Routes are 1:1 with `api` functions**, JSON in/out using the same serde types, under
  `/api/v1/`. A handler is: parse -> call one `api` fn -> map `api::Error` kind to a status.
  No logic in handlers.
- **Dev console at `/`**: one static HTML page (plain HTML + a little JS `fetch`, bundled
  into the binary with `include_str!`) with the database/jobs/config operations as buttons
  and tables -- status and table counts, directories list/add/edit, media/temp dirs,
  settings, job list with enqueue buttons for each job type, DAT sync trigger. Deliberately
  throwaway: it exists so the REST surface can be exercised in a browser immediately, and
  is replaced by the Dioxus web build when that lands (`docs/plan/gui.md`). Not a place to
  spend design effort.
- **Bind**: `127.0.0.1:<port>` by default; `--bind 0.0.0.0` (or `ROMEXPLORER_SERVER_BIND`)
  to serve the LAN. Port default TBD at implementation (not 3307 -- that's the database).
- **Auth: only network access signs in.** The desktop app's in-process backend never
  presents anything: `ServerState::in_process` makes every request this machine's own admin
  (`Principal::Local`), so a standalone or client install needs no account. Every request on a
  network listener passes `guard` (`server/mod.rs`):
  1. *Private-range source-IP allowlist.* Reject any peer address outside `10/8`, `172.16/12`,
     `192.168/16`, loopback, link-local, `fc00::/7`. Catches the port ending up reachable from
     the internet by accident (a forward, UPnP) before anything else runs.
  2. *Who is calling* (`api::auth::Principal`): the `Authorization: Bearer` header, else the
     browser's `rx_session` cookie, else `?token=` -- the last only on image and download
     URLs (`access::query_token_allowed`), which can't carry a header. The token is either
     the **setup token** (`data/server_token`), accepted only while no admin account can sign
     in and deleted once one can; or a **device token** from signing in, looked up by its
     SHA-256 in `user_tokens`.
  3. *May they* (`server::access`): one table classifying every route as `Open` (static
     files, `ping`, `auth/login`), `Read` (any account), `Upload` (`POST /jobs`, where
     `jobs_enqueue` then admits only a `file_scan` `new_only` for a non-admin) or `Admin`.
     Default deny, and a unit test fails on any route the table doesn't list.
- **Accounts** (`api::auth`, `migrations/0028_auth.sql`): named, no shared passphrase.
  `admin` does everything; `user` reads, syncs and fetches media the server already has
  (a RetroAchievements read with `?refresh` is admin, because it makes the server call RA);
  `user` + `can_upload` may also register new files it copied onto the server's share and
  never change existing ones. The server's own console is for admins: a sign-in that says it
  is for the console (`LoginRequest::console`, sent by `console.html` and by the desktop
  app's Server console tab) is refused to any other account, before a token is made. A
  regular account signing in as a client, to sync, is unaffected. Passwords are argon2id; each sign-in mints a token for that one
  device, stored as its SHA-256, revocable per device or per account, no expiry. A wrong name
  and a wrong password answer alike, in the same time; failures are rate limited per name and
  per address; a password change, demotion or disable signs the account's other devices out;
  the last admin able to sign in can't be removed. Backups keep accounts but never their
  hashes (`api::secrets`), and with no usable admin the setup token works again.
- **First accounts and the way back in.** `data/auth.env` (`admin_username`,
  `admin_password`, `user.<name>`, `upload.<name>`) is applied on start while no admin can
  sign in, checked whole first, and deleted afterwards either way -- a file dropped later
  never resets an admin. `romexplorer_server reset-admin [--username NAME]` asks for the
  MariaDB root password, checks it by connecting as root, and sets an admin's password. The
  `--init-from-config` file is deleted once setup succeeds, and a restart drops the flag.
- **TLS on every network listener** (`server::tls`): a self-signed certificate made on first
  start in `data/tls/` (or `--tls-cert`/`--tls-key`). The first byte of each connection
  decides: a TLS handshake is served; plain HTTP from this machine is served as before; plain
  HTTP from anywhere else gets a `308` to `https://` and never reaches the API. Handshakes
  run on their own tasks with a timeout. With no CA and a DHCP address, clients **pin the
  certificate** (`romexplorer_types::pinning`, SHA-256, signatures still verified): the app
  asks the user to confirm the fingerprint on first connect, and machine-to-machine calls
  store it in `remote_sources.tls_fingerprint` on the first connection that works.
  `--no-tls` is for a server behind a TLS-terminating proxy. A browser shows its warning once.
- **Secrets at rest** (`database::vault`): `data/master.key` (owner-only, created before it
  is written, `ROMEXPLORER_MASTER_KEY_FILE` to move it) seals values as `enc:v1:` with
  XChaCha20-Poly1305, each bound to its field as associated data. `api.env` values may be
  sealed (`romexplorer_server encrypt-credentials` does it in place, line for line) and are
  opened on load; a value that won't open is dropped by name, never passed on. Peer tokens in
  `remote_sources` are sealed on write. A key *file*, not a passphrase or keyring prompt,
  because the server restarts unattended; it protects copies of `api.env`, backups and dumps,
  not someone who can read the data directory. Losing it means re-entering provider keys and
  signing in to peers again.
- **Credentials stored in the database never leave it through a generic reader.**
  `api::secrets::SECRET_COLUMNS` names them (today `remote_sources.token`, each peer's bearer
  token). The routes built for one table already keep them in (`sources` returns `has_token`);
  the generic readers are what used to walk past that, so each consults the list:
  - every export (`sync::json_projection`: peer export, database backup, SQLite cache) writes
    `NULL` in their place, keeping the key;
  - the DB viewer shows `********` when one is set, and refuses to filter or sort on it, since
    a `LIKE 'a%'` filter would recover it a character at a time;
  - `rows::Writer` upserts them as `COALESCE(VALUES(c), c)`, so restoring a backup over a live
    install keeps its tokens. Onto a fresh database, peers come back without tokens and are
    re-entered on the Sources screen.
  A unit test reads `migrations/*.sql` and fails on any column named like a credential
  (`token`, `secret`, `passw`, `api_key`) that is not classified, so a new one cannot be
  exported by default.
- **Explicitly out of scope: reaching the server over the internet.** If wanted, that's the
  user's own VPN into the home network. This project never owns WAN exposure, CA-issued
  certificates or dynamic DNS; its TLS and accounts are sized for a LAN. See "Access while
  away" below for the case that actually matters.

## Multi-machine model

The main collection lives on **one server machine**: the files, its MariaDB, the worker,
and `romexplorer_server`. **Client machines** (where games are actually played) each run
their own embedded MariaDB describing a **partial, local collection**, plus the desktop GUI
and their own worker instance. The server's collection root is mounted on clients (SMB or
whatever the user has); copying a game set down is plain OS file copy of
`collection_root + relpath`, which is exactly the path math the `local_directories`/
`local_files` schema stores (naming convention below).

```
Server:   database ---- romexplorer_server (REST, LAN) <-- browser
              |                                        <-- client GUIs (as REST clients)
          romexplorer_worker

Client:   own database <-- romexplorer-gui (in-process via api)
                        <-- own romexplorer_worker (CatalogSync, CopyToLocal jobs)
          romexplorer-gui ---- REST client ----> server (browse/sync catalog, request copies)
```

REST is "how you reach a database that isn't in your process" -- the GUI never uses it for
its own local database, only for the server's. A browser has no other option, which is why
the same REST surface serves both.

**`local_` / `remote_`: a symmetric naming convention, applied on every machine, not just
clients -- but only for the *ownership* tables.** Every machine's own ownership tables --
`directories`, `files`, `rom`, `game` and their junctions -- get renamed to
`local_directories`, `local_files`, `local_rom`, `local_game`, etc. (from
`migrations/0001_schema.sql`'s current unprefixed names). A machine that has synced *from*
another machine gets a `remote_`-prefixed mirror of those **same ownership tables only**
(`remote_directories`, `remote_files`, `remote_rom`, `remote_game`, and their junctions) --
mechanical rule: prefix + exact source table name. One machine's `local_files` **is** the
same data as another machine's `remote_files` once it syncs -- same shape, different role,
not two different schemas. File manipulation (scan, hash, convert, archive, copy) always
targets `local_*`, uniformly, whether that machine happens to be the "server" or a "client"
in a given relationship -- `remote_*` is always a read-only cache, never written by that
machinery.

**`content`, `hashes`, `release`, `title` (and `genre`/`franchise`/`company`/the
provider-cache tables/`sources`/`platforms`) stay unprefixed and are never mirrored at
all** -- confirmed: they're universal catalog/reference data (DAT/provider-sourced), true
regardless of which machine learned about them first, not machine-owned like the ownership
tables. This is more than just "no rename" -- it means sync doesn't need `remote_release`/
`remote_title`/`remote_content` mirrors either: a `remote_game`/`remote_rom` row references
the exact same shared `release_id`/`title_id`/`content_id` a `local_game`/`local_rom` row
would, once sync has upserted whatever reference rows the client didn't already have (same
content-addressed upsert-by-identity logic `import.rs` already uses for DAT imports -- these
rows can't conflict or duplicate, they're keyed by hash/name identity). **This is what makes
the "do I have this title locally?" UI question a single simple join**: for a given
`release`/`title`, `LEFT JOIN` into `local_game`/`local_rom` (do I own it) and separately
into `remote_game`/`remote_rom` (does the server have it) -- both point at the same
`release_id`/`title_id`, so there's no separate remote-reference lookup needed, just
presence-or-absence in each ownership set. Exactly the case the user asked to confirm:
browsing a title/release always shows whether it exists in the local collection, the
server's collection, both, or neither.
**Real cost, not a free rename:** the ownership-table rename touches already-shipped,
tested schema and every existing Rust reference to it across `src/database/` (`lookup.rs`,
`import.rs`, `virtual_collection.rs`, the seed/migration code, and more) -- an actual rename
pass, worth doing now before more code is built on the current names.

**Sync is a mirror (of ownership only), not a merge.** `CatalogSync` does **not** pull the
server's rows into the client's own `local_*` tables (an earlier draft of this design did
exactly that, distinguishing "known but not copied" by the absence of local `files` rows --
wrong, it conflates the server's catalog with the client's own ownership records). Instead:

- Sync (a) upserts any `release`/`title`/`content`/`hashes` rows referenced by the server's
  ownership data that the client doesn't already have -- into the shared, unprefixed
  tables, not a mirror -- and (b) imports the server's `local_*` ownership tables **as-is**
  into the client's `remote_*` mirror, a straight bulk copy. Neither step touches the
  client's own `local_*` ownership tables, which stay the sole ground truth for what's
  actually on that machine's disk.
- The GUI (or `api::catalog`) presents **one unified collection view** by joining
  `local_*` + `remote_*` against the shared reference tables **locally, in SQL, after
  sync** -- no network call per screen. View behavior (local-wins-on-overlap vs. show-both,
  dedup, which fields to prefer) is user-configurable via `settings`, the same pattern as
  `metadata_source_priority`/`title_display_mode` in `docs/plan/database.md`.
- **Consequence: the live REST surface shrinks to almost nothing.** Once synced, browsing,
  searching and picking what to copy are local joins -- no round trip. What's actually left
  needing a live REST call: the sync pull itself, on-demand media streaming (cover
  art/video/audio/PDF bytes -- too large to bulk-sync wholesale), and remote job-trigger
  calls (enqueue a server-side prepare-for-pickup job, see below; trigger/poll a sync).
  This is also why the GUI should feel snappy even against a remote server. What *is*
  bulk-synced about media is the server's `media` rows and their title/release junctions,
  into `remote_media` / `remote_title_media` / `remote_release_media` -- the list of what the
  server holds -- so a client fetches art from the server by its media id (`media_sync`)
  rather than from a provider, has the server fetch what it lacks (`POST /media/fetch`,
  `/media/{id}/refetch`: the server is the household's art cache), and calls a provider
  itself only while the server is offline.
- `JobType::CatalogSync` (client-side job): calls the server's REST API, bulk-pulls the
  referenced reference rows plus the mirrored `local_*` ownership tables for a configured
  scope, upserts the client's shared reference tables and replaces the client's `remote_*`
  tables. Durable, same as every other long-running operation here.
- `JobType::CopyToLocal` (client-side job): for rows the user selected from the unified
  view, resolve `collection_root + relpath` from the `remote_*` mirror, copy each file
  from the mounted server root to the local collection root (bytes never pass through
  Rust/REST), then write the real `local_files` rows.

**Convert/archive/extract on copy: client-side or server-side, chosen per copy.**
`CopyToLocal` isn't always a byte-for-byte copy -- the user may want it
converted/extracted/archived (e.g. ISO -> CHD) in the process, and picks which machine does
that CPU-bound work:
- *Client-side* (no new backend work): plain OS copy of the raw source, then the client's
  own worker runs the already-existing `rom::operation::Single` locally with the
  desired args -- the same machinery `Single` already provides, just invoked by
  `CopyToLocal` instead of a user-facing scan.
- *Server-side*: the client can't enqueue directly on the server's own `jobs` table (no DB
  access there, only REST), so this needs a REST-triggered job on the server's worker (e.g.
  `JobType::PrepareForPickup`) that runs the same `Single` machinery against the
  source, writing the result into the server's own `temp_dirs` rather than mutating the
  main collection. The client polls job status over REST, then OS-copies the prepared
  output once ready; the server cleans up its temp file afterward.
- Both paths reuse `rom::operation` unchanged -- purely an orchestration/job-payload
  question (where the existing stages run), not new conversion code.

**Access while away: a snapshot through cloud-sync, not a WAN-exposed API.** `CatalogSync`
gets a second source, same `remote_*` target either way: live REST pull while on the LAN,
or import from a snapshot file when the server isn't reachable -- the client tries REST
first, falls back to the newest snapshot. The snapshot is a deliberate **export** the
server writes periodically (the same mirrored `local_*` table data sync already pulls --
never the actual ROM/BIOS files, never large media) into a folder an ordinary cloud-sync
client (Dropbox/Drive/OneDrive/Syncthing -- any of them, nothing Dropbox-specific) already
carries to the user's other devices. **Never sync the live MariaDB data files themselves**
this way -- a generic file-sync tool has no notion of "wait for a consistent moment" and
can catch InnoDB files mid-write and corrupt them; a real export (a compact binary in the
style of `tools/biosdb_gen.rs`/`BiosDb`, or compressed JSON -- format undecided) is the
safe version of the same idea. This removes the need to ever expose REST to the WAN --
"access while away" is solved by the snapshot instead. Clear boundary: this solves
*browsing* while away, not *copying* while away -- files still need LAN or VPN access to
where they live. Explicitly out of scope: reaching the server over the internet at all --
if wanted, that's the user's own VPN, not something this project builds or manages.

Left for a focused pass once the first REST slice exists: whether `remote_rom`/`remote_game`
alone are enough for the unified view or the junction tables need mirroring too (e.g.
`remote_rom_files`, `remote_game_roms`, `remote_release_games`), sync scope/granularity
(whole catalog, per platform, on demand), incremental vs. full resync (including how a
reference-row upsert during sync interacts with a client's own independent DAT imports),
the unified-view join's exact SQL/settings shape, `PrepareForPickup`'s payload/cleanup
policy, the snapshot format and export trigger, and an optional "queue a copy request while
away, the server acts on it once home" file-based extension.

### Divergences from this document, as built

Three, each deliberate:

1. **`JobType::CopyToLocal` is `CopyFiles`.** That name presumes a client pulling from a
   server; the same machinery copies local files onto a USB stick and (later) up to a
   server. Direction is a payload field (`CopyMode`), not the job's identity.
2. **A copy registers what it wrote, and hashes nothing to do it.** This document has the
   job "write the real `local_files` rows", which is right; an earlier pass of this work
   handed the task to a `file_scan` instead, and that was wrong. A scan re-derives every
   file's identity by hashing it with the full algorithm set -- the correct thing when you do
   not know what a file is, and pure waste when you do. A copy does: it carries each file's
   `content_id` from the ownership row it came from, and `content` is shared catalogue data,
   identical on both machines. So `api::copy::run::register` writes the rows directly, through
   `api::scan`'s own three writers so there is one definition of what a row looks like, and
   carries the release link over so a copied game shows as held with no hash match to wait
   for. Checking that the bytes *landed* is a separate and much cheaper question, answered by
   one quick algorithm when `verify` is on.

   **An upload is the exception that proves it.** There the destination is the server's, and
   its `local_*` rows are the server's to write -- a client inventing rows in its `remote_*`
   mirror would be claiming something the next sync contradicts. So the client asks: it maps
   each destination directory back through `mount::to_remote` (the inverse of the resolver,
   round-trip tested), asks the server which of *its* directories that is, and enqueues a
   `file_scan { new_only }` there. That is also the mandatory rehash the upload needs, taken
   by the machine that now holds the bytes rather than the one that sent them.
3. **`PrepareForPickup` is not built.** Client-side conversion (copy, then the client's own
   `rom::operation::Single`) covers the stated need. The server-side variant needs a
   REST-triggered job on the server's worker, a temp-file lifecycle and a cleanup policy,
   and this document leaves its payload undecided.

Also new here rather than in the original design: the mount is keyed on a **path prefix**
rather than on a collection-root row, because the share is often mounted at the *parent* of
several roots and no `remote_directories` row exists for that parent.

## Order of work

1. **`src/api/` + `romexplorer_server` together.** Build the intermediate layer and the
   REST server as one slice:
   - `src/api/`: `App`, `Error`, `status`, `settings`, `jobs` (typed payloads),
     `directories` (with the new `database::directories` CRUD), `storage`, `catalog`
     (wrapping what `lookup.rs` already has), plus `dats`, `providers`, `sync`, `copy` --
     all operations needed by any UI, not phased by consumer. Unit tests on payload
     validation and serde round-trips.
   - `romexplorer_server`: bring-up, middleware (token + IP allowlist), routes for all of
     `api`, the dev console page. This is the first thing visible in a browser, and
     exercises the full `api` surface.

2. **`romexplorer_worker` onto `api`.** The worker's job handlers dispatch through `api`
   functions (DAT sync, provider query, bulk sync, etc.) instead of hand-wiring `database`
   calls. `FileScan` and `HashMatch` handlers move from stubs to real dispatch.

3. **`romexplorer_dbtool` onto `api`.** Behavior unchanged; it just stops hand-wiring
   `database` calls and uses `api` instead.

4. **`romexplorer-gui` (Dioxus desktop).** Built last, with all `api` surface already
   proven by the REST server and the worker. The GUI is feature-rich and carries all the
   web-UI experience -- screens for all `api` operations, media playback, PDF reader,
   etc. -- and tests against the same `api` the REST server already exercises, so there's
   no "build the GUI, discover the backend is broken" surprise. See `docs/plan/gui.md`.

## Verification

- `cargo test --lib api` for payload validation and serde round trips; no network.
- `tests/src/test_database.rs` gains a phase that, against the same throwaway server,
  calls the `api` functions directly (enqueue via typed request -> claim -> the stored
  payload matches `jobs.rs`'s documented shape; directories insert/list/resolve).
- `romexplorer_server` started against the `app_root_ex` database with the 302-game
  virtual collection: the dev console shows table counts, lists directories, enqueues a
  `FileScan` (stub-processed by the worker, so it just completes), reads and writes a
  setting. Then the same from another machine on the LAN with the token -- and a request
  without the token, or from a non-private source address, is rejected.
- No real names/paths/hashes in any tracked file; the token file is git-ignored and never
  appears in logs or responses.

## Implementation status

Landed (module doc comments carry the detail):

| plan item | where | notes |
|---|---|---|
| `local_*` rename | `migrations/0001_schema.sql`, `virtual_collection.rs`, `romexplorer_worker.rs` (`db_optimize`), `romexplorer_dbtool.rs` (`counts`), `docs/plan/database.md` | `directories`/`files`/`rom`/`rom_files`/`rom_rahash`/`game`/`game_roms`/`release_games` -> `local_*`; constraint/index names prefixed `l...` so a later `remote_*` mirror can't collide. Pre-release: `0001` was edited in place, so an existing data dir's recorded migration checksum no longer matches -- recreate the data dir (or point `ROMEXPLORER_DATA_DIR` at a fresh one) rather than migrating. |
| `database::directories`, `database::storage` | new modules | `local_directories` upsert/update/list/get/soft-delete + `resolve` (nearest configured ancestor by path prefix, then inherit up the parent chain). Hierarchy (multiple non-nested roots, everything else inside one, derived `parent_directory_id`, recursive config-only removal via `DELETE /directories/{id}?recursive=true`) is enforced in `api::directories` -- see docs/plan/database.md's `local_directories` entry; `media_dirs`/`temp_dirs` with the tier1/tier2 single-row rule in Rust. |
| `database::jobs` additions | `jobs.rs` | serde on `JobType`/`FileScanMode`; `list`/`get`/`retry`/`status_counts`, `JobRow`. `settings.rs` gained `title_display_mode`/`metadata_source_priority` keys, `KNOWN_KEYS`, `list_all`, `delete`. |
| `src/api/` | `mod.rs` (`App`, `Error`/`ErrorKind`), `status`, `db` (generic viewer), `settings`, `jobs` (typed `JobRequest` -- the deferred payload validation), `directories`, `storage`, `catalog` (titles/releases/games/files/lookup, every list paged + allowlisted sort + substring filter, joined against `local_*` so rows say "held locally"), `providers`, `dats`, `setup` (`SetupFile`, `ensure_server_token`) | `sync.rs` and `copy/` are built; `PrepareForPickup` (server-side convert-before-copy) is not. |
| `src/server/` + `romexplorer_server` | `server/mod.rs`, `server/console.html`, `src/bin/romexplorer_server.rs` | axum 0.8; routes 1:1 over `api` under `/api/v1/`; `guard` middleware = private-range source-IP allowlist + bearer token (constant-time compare), `/`, `/api/v1/ping` token-exempt; the console page is one vanilla HTML/JS file (`include_str!`): status + hash lookup, Titles -> title detail -> Releases -> release detail (catalog contents, local copies with roms/files, archive rows show members on hover), Games, Files (+ file detail), Directories/Storage/Settings forms, Jobs (full/quick/rehash/hash-match buttons per configured directory, DAT sync, DB optimize, auto-refreshing list, retry), Providers, DATs, DB viewer (any table, sort/filter/page). Default port 7337, bind 127.0.0.1; starts the bundled `mariadbd` if the configured DB port doesn't answer. |
| integrity check | `src/api/integrity.rs`, `api::copy::{comparison_hash, check_file, CheckOutcome}`, `JobType::IntegrityCheck`, setting `integrity_populate_hash`, `migrations/0026_integrity_and_copy_verify.sql`, `GET|POST /api/v1/integrity`, Jobs > Integrity check, schedulable | Reads each held file **once** and takes two answers off that read. **Compares** against whichever hash the catalogue already records for that content and is cheapest here -- `rom::hash::fallback_order`, the same order `FileData::integrity` already uses: crc32, xxh3_128, then sha1/sha256 where the CPU has hardware SHA and md5 where it does not, since MD5 has no hardware path and beats software SHA. Not a setting, because comparing against an algorithm the catalogue did not already hold cannot settle anything. **Populates** `integrity_populate_hash` (xxh3_128 by default) off the same pass, recorded *only after* the comparison passed -- a hash taken from a file nobody has confirmed must never become the catalogue's idea of what that content is, since `hashes` hangs off `content` and other copies share it. Content with nothing recorded at all is `Unverifiable`: not read, not written, counted. Deliberately not `file_scan { rehash }`, which re-derives each file's whole identity when the `content` row already says what it is. Least recently checked first; a file gone from disk is soft-deleted and named as missing, not called corrupt; only a file that matched is stamped, so a mismatch stays at the front of the queue. `limit` / `older_than_days` keep a run short, which is what lets it run often. |
| create / check cue, gdi, m3u | `rom::sheet_build`, `src/api/sheets/{mod,rename,run}.rs`, `JobType::{SheetCreate,SheetVerify,M3uVerify}`, `migrations/0027_sheets.sql` (`local_sheet_check`), setting `sheet_style`, `POST /sheets[/preview]`, `/sheets/verify`, `/sheets/m3u/verify`, the button on the rom page and the game card, Jobs > Disc sheets and playlists | A cue/bin set is the one dump shape whose index is a separate file, so losing it loses the dump: the tracks are there, and nothing says what they are or what order they go in. **Conforms to the CHD path by construction**: `chd_cd` builds a `Vec<SheetTrack>` from a CHD's table of contents and renders it through `Sheet::write`, and this builds the same vector from files on disk and renders it through the same writer, so no cue or gdi text is formatted anywhere but `detect::sheet` (whose round-trip tests hold it byte-identical to `chdman extractcd`). `cd_layout` itself is not reusable -- it reads geometry from a `ChdReader`, and loose tracks have no table of contents -- so recovering that geometry is `sheet_build`'s whole job. Length is arithmetic, type is a two-byte read (the sync pattern, with the mode in byte 15; audio is what lacks it), and **the gaps are not recoverable from the bytes at all** -- a cue may carve a pregap out of the track file (`INDEX 00`) or declare it absent (`PREGAP`), Redump uses both, and they produce identical bytes on disk. So it renders every convention and keeps whichever reproduces what the catalogue records for that sheet, turning an unanswerable question into a lookup for any dump a DAT covers. Size alone never confirms (two conventions of one disc are a few digits apart and collide constantly); `Rebuilt::matched` is the only thing that claims confirmation. A GD-ROM needs no search -- tracks 1-2 are the single-density area from frame 0 and track 3 opens the high-density area at a fixed frame. **The unit is the dump, not the rom**: with its cue a cue/bin set is one `local_rom` (the cue is `main_path`, the tracks are its files), and without it nothing groups the tracks, so each `.bin` is scanned as its own rom and they share a game only through their release -- the rom button resolves upward, and creating the cue collapses them back into one on the next scan. Eligibility is asked per DAT source rather than pooled, since Redump's cue/bin dump and mame-redump's CHD describe one release and a line missing from one is not missing from the other; among equally-whole sources the one that names the sheet wins, or a source listing no cue looks complete and hides the dump that is short of one. Every other outcome is a **disabled button with a reason**, never a silent absence. Checking is **two independent verdicts**: structure (it parses, names files that are there, divides into whole sectors) and hash (byte-identical to the DAT's), kept apart because a sheet can play perfectly and not be Redump's -- that is `warn`, never `bad`, and `hash_known` separates "nothing to compare against" from "compared, and it differs". Renaming is the one operation in the app that **moves the user's own ROM files**: the whole old-to-new table is computed first and changes nothing, `rename_confirmed` is required, it is off unless asked, never offered from the whole-collection job, and refuses per row (a target that exists, two sources landing on one target, a name the filesystem would mangle) so one bad name does not hide twenty good ones. A set whose names already agree is left alone even when they are not the DAT's. The rename runs before the write (the sheet must name the files as they will be) and the file moves before its row (a row pointing at a vanished path is something the scanner understands; one pointing at a path that now holds something else is not). Both verifies are schedulable like the integrity sweep, least-recently-checked first; `sheet_create` deliberately is not. Per file, `POST /sheets/files/{id}/{verify,fix}` re-check one sidecar now and try to repair one whose only fault is its hash. **What a repair can reach is exactly one thing**: a sheet's bytes are its track names plus its gap notation, and only the notation is the app's to rewrite -- so the fix is the same convention search, now with something to aim at. Track names that are not the DAT's make a match impossible however the sheet is written, since the names in a sheet have to be the names on disk; that refuses, names the offending files, and points at the rename operation rather than quietly reaching for it. Every refusal says in the same breath that the disc still works, because it does and because "doesn't match the database" otherwise reads as a fault someone will go and re-dump a good disc over. The button is only offered where it could help (`structure_ok` with a DAT hash), and the cell is a real component rather than a `Column::render` closure -- that closure runs inline in `Grid`'s own scope, so per-row `use_signal` there would drift onto other rows as the list sorts and pages. |
| copy to a folder | `src/api/copy/{mod,mount,plan,run}.rs`, `src/api/naming.rs`, `common::platform_names`, `JobType::CopyFiles`, `migrations/0025_copy.sql` (`remote_mounts`, `platform_output`, `platforms.romm_slug`), `POST /copy` + `/copy/preview`, `GET|PUT /copy/defaults`, `GET|PUT /naming/folders`, `/sources/{id}/{roots,mounts}`, `/mounts/{id}[/check]`, GUI Config › Copy + "Copy to…" on sets/titles/releases/games/roms/files | Right-click or multi-select at any of the six levels and copy to a folder. A selection resolves down to files (a title means its top-ranked release unless asked otherwise, reusing `sets::top_releases`), a release picks one holder -- **completeness before locality**, then the user's preference, then main before peer, then lowest id -- and a game expands to its roms; `local_rom_files` keeps a cue with its tracks and a directory dump with its tree, and a game's `.m3u` is rewritten to the names the copy produced. Remote files come through `remote_mounts`: a path prefix as the *source* machine writes it, mapped to a local mount point, longest prefix wins, case-folded only for `\`. Bytes are plain OS file copy, never REST. Dedup is by `content_id` per destination path; the same content wanted at two paths is copied twice and reported rather than hardlinked (a hardlink survives neither exFAT nor a deletion of one copy). **Five modes** (`CopyMode`), each a different answer to five policy questions and nothing else -- whose holdings count as "already got it", whether the result is registered here, whether checking is optional, whether an existing destination may be overwritten, and whether the user is asked about one. The table is asserted whole in one test, because "in-server never overwrites" quietly becoming "in-server usually doesn't" is exactly the kind of drift five scattered predicates invite. `to_collection` (server -> here) and `to_external` (SD card, phone, USB) are the two the client offers by default; `in_local` converts within this machine's own collection; `in_server` does the same on the server, from the server's own UI; `upload` sends this machine's files up through the mount. `api::copy::available_modes` is what the dialog offers, so a mode that cannot apply here is absent rather than present-and-always-refused: `in_server` is missing on a client (it rearranges a collection whose catalogue is not this machine's), `upload` is missing with no mounted server. Reaching the server's own UI needs no new surface -- the server serves the same Dioxus build at `/app/`, so browsing to it *is* the server console for this purpose. `verify` is orthogonal except where a mode refuses to let it be: `in_server` always checks, and `upload` never does here, because a hash the sender computed says the bytes left correctly, not that they arrived -- the server is asked for a `file_scan { new_only }` instead, which is that mandatory rehash taken by the machine that now holds the bytes. `to_collection` skips content this machine already holds and **registers what it wrote directly**, reusing the source's `content_id` rather than re-hashing to rediscover it -- which also means every `hashes` row already hanging off that content applies to the copy untouched -- and carrying the release link over so the copy shows as held immediately. The two `local_files` check stamps mean what `FileData`'s two methods mean: `last_full_check` is `FileData::hash` (every algorithm, archive members and all) and only a scan earns it, so a copy that reused a known `content_id` sets neither, and `last_integrity_check` (`FileData::integrity`) only when it actually read its output back. The one hash that does not come for free is `ra_hash`: the schema hangs it off the *rom* rather than the content, because it describes a playable unit that may span several files, so `carry_ra_hashes` copies the `local_rom_rahash` / `remote_rom_rahash` junction onto the new rom. A **converted** rom carries it too: the RetroAchievements hash is computed from the decoded stream rather than the container (`rom::hash::rahash_file` resolves a `.chd` natively, exactly as it resolves a `.cue`'s `.bin` tracks), and this copy only ever re-containers -- `convert_one` runs with `StageSwitches { convert: false, .. }`, so the payload bytes are invariant by construction. The one refusal is an **incomplete** rom, which is not the playable unit the hash describes; a track folded into a disc container is not missing for this purpose, since its bytes are inside the output (`PlannedCopy::folded_into_carrier`, excluded from both sides of the completeness count). `to_external` does neither. **What checking a copy can even mean depends on what the copy did to the bytes** (`run::check_kind`), and asking the wrong question returns a confident wrong answer rather than an error. Unchanged bytes get the **whole-file** check. A copy that re-containered -- cue+bin written as one CHD, a 7z rewritten as a zip -- will not equal anything recorded, so it gets the **content integrity check** (`copy::content_check`): decode the output once, compare each member against the `content_contents` the source content had, and on success give the new container its own `content` row with the scan's full hash set plus member links. That full hash set is the "hash the archive too" half: it earns the `last_full_check` stamp and makes the *next* integrity check a cheap whole-file comparison instead of another decode, and it is what lets a DAT match the new archive at all -- which it could not while the row described the source's different bytes. The same routine covers the raw-to-archive direction, where the source content itself is the one expected member, and the extract direction, where the output is a loose file compared as that member. A copy that converted the **payload** (header add/strip, byte order, decryption) has nothing it should equal, so it is not verified at all: `content_check::rehash` hashes it to give it an identity and a `hash_match` follows to say what it now is. That path is unreachable today and written anyway -- `PAYLOAD_CONVERSION` is one constant three places must agree on, and two of them (`carry_ra_hashes`, `check_kind`) fail *silently* if they do not, recording files as things they are not. `verify` reads each file back through the same `check_file` the integrity sweep uses -- compared against a hash the catalogue already held, with `copy_populate_hash` filled in off that read once it matched; off is the fast path, and a file that fails is named, left unregistered, and causes nothing to be written. A converted output is the one thing that must be hashed either way -- its bytes are new -- so `Single` taps the same single algorithm off the stream it is already writing. An existing destination is resolved by mode: `in_server` and `upload` **never** overwrite -- the file is left alone and named in `blocked_existing` while the rest of the copy carries on -- and `in_local` asks, per file, with overwrite-all / skip-all over the preview's `conflicts` list, where **unanswered means skip**, never destroy a file nobody said to destroy. The two are mutually exclusive by construction and asserted so. The run re-applies the same rule against the destination as it is at that moment, since the preview and the copy are not one transaction. Layout is one of four naming schemes per platform (pack name with manufacturer / display name / IGDB slug as RomM matches folders / EmulationStation short name) plus per-platform overrides and combine groups for the platforms this app splits and a folder layout does not. Conversion is the first production caller of `rom::operation::Single`, in a per-file scratch directory, degrading to a plain copy where the native library is missing. Resumable: every write lands on `<dst>.recopy-part` and is renamed into place, and an existing destination matching by size and the cheapest recorded hash is skipped, so re-enqueueing a died-halfway job is the resume. Cancellable through an `AtomicBool` a sibling task drives from `jobs.cancel_requested`, because the worker's task abort cannot interrupt a `spawn_blocking` copy loop. |
| tests | `tests/src/test_api.rs`, unit tests in `api::{jobs,settings,setup}`, `server` | throwaway MariaDB: directories inheritance, storage single-row rules, settings validation, typed enqueue -> claim -> payload shape, retry, DB viewer sort/filter, empty-catalog reads, then real HTTP on loopback: ping exempt, 401 without/with wrong token, 201 enqueue, 404 shape, viewer rows. |

| `api::scan` + worker on `api` | `src/api/scan.rs`, `romexplorer_worker.rs` | The worker now holds an `api::App` (step 2, partially) and `FileScan`/`HashMatch` are real, minimally: `run_file_scan` walks with `rom::scan::scan` (platform: the request's, else the directory's effective setting -> `Forced`; else `SeparatedBySubdir` with folder-name hints), hashes each candidate's files (`InputSet::resolve` gives a sheet its tracks and a bundle its files) on the blocking pool with crc32/md5/sha1/sha256, dedups `content` by strongest hash exactly like `import.rs` (so a known dump lands on the DAT's own content row), upserts `local_files` by `path_hash`, one `local_rom` per candidate and one `local_game` per rom; `full` soft-deletes the directory's rows first, `new_only` skips live paths, `rehash` re-reads known rows and marks missing ones. `run_hash_match` (`to_dat`) links each unlinked game to the release whose `release_contents` cover most of its files. Not yet: archive members (`content_contents`), `.m3u` grouping into one game, deep detection, Hasheous/playmatch (logged, skipped), progress reporting. |

| console round 2 | `src/server/console.html`, `api::catalog`, `api::sets`, `api::storage`, `import.rs`, `models.rs` | Tabs regrouped: Status / Collection (Titles, Releases, Games, Files, Sets) / Jobs / Config (Directories, Storage, Settings) / Data (Providers, DATs, DB). Region and language are separate columns everywhere; structured DAT `region=`/`language=` attributes are normalized at import to the same codes name parsing produces (RA's `region="English"` becomes `languages=en`; "USA"/"Japan" become `US`/`JP`; unknown words kept). `language_display` setting: code / English name / native name (`Language::english_name`/`native_name`). Badge colors per category/platform/region/language/source/archive format, overridable via the `ui_colors` setting (platform pastel by default, category stronger; raw ISO whitish, chd light blue, rvz red, zar green, gcz purple, wia pink, zip/7z yellow; `.iso` files are tagged `raw_iso` at scan). Titles filterable by linked provider, with provider icons per row; title/release pages list each provider's web page (id-addressed URLs; IGDB via the cached raw response's `url`/`slug`, else a site search). `view` = local / server (empty until `remote_*` exists) / collection / collection-local-first (default, held-locally rows first). List or grid (box art by default, `grid_media_type` setting) for titles/releases/games; `/api/v1/media/{id}` streams a media file, looked up in the tier 1 then tier 2 media cache before the original (`storage::resolve_media_path`, copies into tier 1 on a miss); the token may come as `?token=` for `<img>`. Collection sets (`collection_sets`/`collection_set_releases`): right-click any release row (Releases, a title's releases, Games, a set) to add it to a set or a new one; a set page reorders/removes. Temp dirs: `storage::pick_temp_dir`/`reserve_temp_dir` choose tier 1 then 2 by free space (`sysinfo`) minus in-process reservations, keeping the `temp_dir_margin` setting (max of a percent and a byte floor); `estimated_extracted_bytes` sums a zip's member sizes, ×3 for other containers -- the hook a `Multiple` will call per input before spawning its `Single`s. |

| badge icons | `console.html`, `server/mod.rs` (`/icons/{file}`), `AppPaths::icons_dir` | Region and language badges carry an emoji flag built from the ISO code (offline; needs a color emoji font). Platform badges carry a hardware silhouette: `ui_icons` setting URL first, then a file named `<platform-slug>.png|svg|webp|jpg|gif` in `app_root_ex/assets/icons/` (served token-exempt at `/icons/<file>`, plain file names only -- drop a ScreenScraper-style pictogram pack there), then a built-in simplified inline-SVG silhouette (NES/Famicom/SNES/N64/GameCube/Wii/Wii U/Switch/Game Boy/GBA/DS/Virtual Boy/Master System/Genesis/Saturn/Dreamcast/Game Gear/PSX/PS2/PS3+/PSP/Vita/Xbox/360/PC Engine/Neo Geo/Atari/Jaguar/CD-i/arcade/computer -- shapes only, no marks), else a generic cartridge/disc/handheld/computer glyph. Platform default colors come from `platforms.family` (Nintendo reds, Sony blues, Microsoft greens, Sega yellows, microcomputers whites, others tinted greys), one fixed variant per platform. |

| worker lanes | `romexplorer_worker.rs`, `jobs::{lane_of, peek_pending, claim_by_id}` | The worker is the app's only outgoing path to providers (the server/API only enqueues). It runs jobs in lanes -- one job at a time per lane, lanes concurrently: `provider:<name>` per provider (so each client's own rate limiter, e.g. IGDB's 4/s `RateLimitLayer`, sees all traffic to that provider as one serial stream), `cpu` for scan/hash-match/optimize, `dat` for DAT downloads. The scheduler peeks pending jobs oldest-first and claims only those whose lane is idle. One worker per set of credentials, or the per-process limiters add up. Jobs list/UI show the lane. |

| request coalescing | `api::batch::chunks` in worker `provider_query_ids`; `api::batch::Coalescer` (built and tested, no caller yet) | Ids for a provider with a list endpoint go out as list requests split to the provider's maximum (`chunks`: IGDB 100 per request, in `provider_query_ids`). `Coalescer` -- a queue that merges concurrent lookups of the same key and flushes within one rate-limit slot (`for_rate(max, per_second)`: IGDB 4/s -> 250 ms) -- is not what runs there, and should not be: the id list arrives complete and already deduplicated (`BTreeSet` in `run_hash_match`) and the provider lane guarantees no concurrent caller, so it would add a 250 ms wait per flush with nothing to merge. It waits for a call site that has concurrent independent lookups. `run_hash_match` itself is DB-only and does no batching. Hash match with `to_hasheous` runs in the `provider:hasheous` lane (Hasheous is single-hash, so serial through its client), skips content already looked up within `cache_ttl_days`, and -- with `to_playmatch` -- collects every cross-ref id it saw and enqueues **one** `provider_query` per provider carrying the whole `provider_ids` list; that job runs in its own provider lane and fetches in chunks (IGDB 100 ids per request via `get_games_by_id`; RetroAchievements/SteamGridDB/MobyGames/ScreenScraper have only by-id calls, so per id, still serial in their lanes; TheGamesDB/EmuReady/Hasheous have no by-id fetch in the vendored clients, so their cross-refs stay as links only). The vendored HTTP retry policy now also retries HTTP 429, waiting `Retry-After` when given (else its backoff schedule), and IGDB's limiter window is 4 per 1100 ms -- a 10% margin against the two sliding windows drifting. |
| provider metadata | `database::providers::metadata`, `cache::ensure_hasheous_release`, `api::dats::sources`, worker `provider_query`/`provider_bulk_sync` | What a provider knows about a game now lands in the source-tagged title tables: `metadata::apply_igdb_game` writes genres/franchises/companies (developer/publisher/porting from `involved_companies`) and maps IGDB `game_type`/`category` onto `title.category` (port/remake/remaster/dlc/expansion/bundle/mod), pointing `basegame_title_id` at the local title linked to IGDB's `parent_game`/`version_parent`; the ids IGDB uses for those are resolved by `IgdbNames` in list requests of 100 (one resolver per job/page, cached in-process). RetroAchievements applies `genre_raw` through `ra_genre`, MobyGames its `genres`, ScreenScraper `editeur`/`developpeur` as companies, Hasheous its publisher. Applied after every by-id fetch (`provider_query_ids`, cached ids re-read from their raw files instead of refetched), after an IGDB name query links a title, and per page in `bulk::sync_igdb_platform`. Hash match with `to_hasheous` now also sends DAT-identified games whose release/title has no provider link yet (`hasheous_for_linked` in the summary), since the cross references are how a DAT-matched game gets its IGDB/RA/... ids. A Hasheous hit for content no DAT covers becomes a release + title under the `hasheous` source (`ensure_hasheous_release`: platform from the signature's system name, else the rom's; publisher recorded), the local game is linked to it and the cross-references re-linked so the playmatch follow-ups have something to attach to; cached hits are rebuilt from their raw files (`hasheous_record_from_cache`). `ProviderBulkSyncRequest.platform` is a `PlatformSelect`: `"all"` walks every platform the catalog has releases for (IGDB ids < 2048, RA consoles it knows), one platform's failure logged and the rest continued. `GET /dats/sources` returns, per source, what the import actually produced (`titles_in_db`/`releases_in_db`/`content_in_db`) next to the DAT-declared game/file counts; the console's DATs tab shows both groups. |
| manual match + name fallback | `api::links`, `cache::search_by_name`/`best_hit`, worker `provider_query_titles`/`after_link`, `GET /providers/{p}/search`, `POST/DELETE /catalog/{titles,releases}/{id}/links[/{provider}]` | Hash match with `to_playmatch` now also enqueues, per searchable provider (IGDB, ScreenScraper, SteamGridDB, MobyGames, EmuReady, TheGamesDB), one `provider_query` carrying `title_ids`: every title of the matched games that still has no link to that provider (Hasheous often reports an empty id for a provider that does have the game). The worker looks each up by the title's own name + platform, links the best hit (normalized-name equality, else the provider's first result; title-tier -> the title, TheGamesDB -> the title's releases on that platform) and applies metadata + art from what the search cached. Manual match: the console's "manual match…" dialog on title and release pages searches a provider by name (synchronous, through the rate-limited client) or takes a pasted id, links it with `confidence = manual` (a title-tier provider linked from a release attaches to its title; a release-tier one linked from a title attaches to its releases), unlinks, and queues the by-id fetch. Release pages list the title's provider links too (`via: "title"`), since title-tier providers carry per-region names and art. |
| release rank, covers, media rule | `catalog::ReleaseRank`, `title_cover_sql`, `settings::{region_priority, language_priority}`, `media::{release_targets, attach_media, has_media, igdb_cover_urls}`, `franchise_media`/`keyword`/`title_keywords`/`keyword_media` | Releases of a title come in rank order (also `sort=rank` anywhere): the user's manual order first (`title_releases.sort_weight`, dragged on the title page; `POST /catalog/titles/{id}/release-order/move`, `DELETE …/release-order`, Edit › Manual order via `GET /catalog/titles/release-orders` and `POST …/release-orders/clear`), then kind (commercial > patched > RA subset), `region_priority` (first region of the release), `language_priority` (best language it has), then edition / version per `release_edition_order` (budget_first: budget reissue > latest `revision_sort` > special edition > aftermarket; versions_first: latest version, budget as tie-break), then RetroAchievements support (content MD5 in `retroachievements_hashes`, or an exact RA link) as a tie-break, then id; releases carry `is_budget` / `is_special_edition` / `is_aftermarket` (whole-group name tags, `name_parse::Edition`); the title's grid art is its best-ranked release's art. Media rule: images and videos belong to releases -- a provider's title-level cover (IGDB `cover` via `get_covers_by_id` in lists of 100, SteamGridDB grid as `cover`, MobyGames sample cover) is downloaded once and linked to every release of the title; RetroAchievements box art goes to the releases linked to that RA game; audio belongs to a title (soundtrack), a franchise or a keyword (arrangements spanning games) -- `MediaTarget::{Franchise, Keyword}`, `media_download` accepts `franchise_id`/`keyword_id`. Art is attached after by-id fetches, after name links and per page in the IGDB bulk sync, skipping targets that already have that provider's art. Games/releases carry verified/missing counts (`dat_verified`, `hasheous_verified`, `missing_files`; `local_verified`, `local_missing`) shown as chips on cards and rows. Not yet: ScreenScraper per-region media (the vendored model has no `medias`), TheGamesDB images, keyword editing UI. |
| PlayMatch + link confidence | `providers::playmatch` (own client), `cache::{playmatch_lookup, playmatch_record_from_cache, platform_compatible}`, `Confidence::{Hash, Name}`, `playmatch_lookups` | The hosted PlayMatch service (`GET /api/v2/identify/ids?fileName&fileSize&md5&sha1&sha256&crc`, one file per call, 4/s, no credentials) is the second hash-lookup source next to Hasheous: `Provider::PlayMatch` (content tier, own cache table + raw files), run by `hash_match` with `to_playmatch` for every content of the targeted games, its `externalMetadata[]` provider ids linked as cross references. Link confidence now says where an id came from: `exact` (the provider's own hash list, e.g. RA md5), `hash` (a hash service's cross-reference for the identified game -- Hasheous/PlayMatch), `name` (a name search whose normalized name equals ours), `fuzzy` (a name search's first result), `manual`. The by-name fallback searches platform-scoped first; when that's empty it retries unscoped but then links only an exact name whose IGDB entry lists our platform or a sibling (`cache::sibling_igdb_platforms`: NES/Famicom, SNES/Super Famicom, GB/GBC, PC Engine/SuperGrafx, Neo Geo MVS/AES, and this project's app-only ids -- Famicom Disk System, Sufami Turbo, WonderSwan (Color), Pico, 64DD, PocketStation, MSX variants -- mapped to IGDB's own platform ids; JP/US names IGDB already unifies, Mega Drive/Genesis, PC Engine/TurboGrafx-16 and CD, Mega CD/Sega CD, Mark III/Master System, need nothing), so a same-named port for another system is never linked. Scoped IGDB searches and the IGDB bulk sync use the same table, so app-only platforms search/sync under their IGDB id instead of being skipped. |
| job priority / fast lanes, page refresh | `jobs.priority`, `db_jobs::enqueue_with_priority`, `peek_pending` (priority DESC, id), `JobRow.lane` `:fast` suffix, `api::links::{refresh_title, refresh_release}`, `POST /catalog/{titles,releases}/{id}/refresh` | Jobs carry a priority (0 = background). The scheduler takes higher priority first, and a priority job gets its provider lane's `:fast` slot, so it runs alongside -- not behind -- a bulk sync in the same lane (both go through the same in-process rate limiter, so the provider limit still holds). Opening a title/release page in the console posts a refresh: a by-id fetch (metadata + art) for every linked provider and a by-name lookup for every searchable provider still unlinked, at `PRIORITY_INTERACTIVE`, deduplicated against pending jobs (same payload). Manual links queue their fetch the same way. Not yet: remembering failed name lookups (a page reopened later re-searches providers that had no match). |
| facets + genre classification | `catalog::ListQuery` (`genre_major`, `genres`/`genre_logic`, `franchises`, `companies`, `keywords` + `*_logic`, `region`, `language`), `GET /catalog/facets`, `providers::ra_genre` (RAChecker port), `metadata::{ensure_genre, set_title_keywords, set_title_perspectives}`, `perspective`/`title_perspectives` | Titles, releases and games filter by major genre, raw subgenres, franchises, companies and keywords (each multi-valued with and/or logic, and = default) and by one region / language; the console renders them as type-to-filter pulldowns with chips. Genre rows sit under one of the 19 RetroAchievements major genres via a native port of the user's RAChecker classification (exact major, exact subgenre/alias, keyword priority, "Other" last) -- applied to every source's raw genre text, so IGDB "Platform", RA "2D Platforming" and MobyGames "Platform" all filter as Platforming while the raw name stays a subgenre. IGDB "Indie" is a keyword, not a genre; MobyGames uses only its Basic Genres + Gameplay categories; IGDB `player_perspectives` and MobyGames' Perspective category are stored separately as perspectives (not shown in the console yet). |
| worker in the server, lifecycle | `romexplorer::worker` (moved from the worker binary), `romexplorer_server`, `server::Lifecycle`, `jobs::release_interrupted` | The lane scheduler is a library module; `romexplorer_server` runs it in-process by default (`--no-worker` to use the standalone `romexplorer_worker`, now a thin wrapper). It's independent of HTTP traffic -- jobs run with no browser open -- and a DB error while polling is retried, not fatal. The process stops only deliberately: Ctrl+C, or `POST /api/v1/server/{shutdown,restart}` (console: Config › Server). Stop order: listener, worker (running jobs aborted and put straight back to `pending`, the try not counted), embedded MariaDB; restart then spawns a fresh copy with the same arguments. `GET /api/v1/server`: pid, start time, whether the worker runs here. The console page is served `Cache-Control: no-cache` so an update never runs old JS against the new API. |
| job progress | `migrations/0002_job_progress.sql`, `jobs::Progress`, `jobs::human_bytes` | `jobs.progress_done/total/message`, written by the worker at most ~1/s (`Progress::report`; `force` for phase changes), reset on claim, shown as a bar in the console's Jobs view. Provider bulk syncs report pages/games/consoles/dump bytes; `dat_sync` reports source i/n, file bytes (via `DownloadOptions::progress`) and files imported. |
| provider request scopes | `api::providers::capabilities`, `ProviderBulkSyncRequest.full`, `bulk::{link_exact_names, sync_thegamesdb_platform, sync_emuready_platform, sync_retroachievements_all}`, `providers::dumps`, `cache::retroachievements_local_search` | Per provider, from each API's docs: keyword (by name; per system for ScreenScraper/EmuReady), platform (one platform's catalog, or every platform the catalog has), full (the whole database). Full only where the API reaches everything: TheGamesDB's official JSON dump (one download, no allowance), Hasheous' MetadataMap dump (~2.6 GB), every RetroAchievements console (~70 calls); not IGDB (its dumps are partner-only), ScreenScraper/SteamGridDB (no listing endpoint). New per-platform paths: TheGamesDB `ByPlatformID` (20/page, monthly allowance), EmuReady `games.get` pages, Hasheous per-platform dumps. RA keyword searches the cached console lists (RA has no search API). `api::jobs::validate` enforces the scopes; the console's Providers tab builds its request form from them. Bulk results link by **exact** normalized name per platform (one query per platform, `Confidence::Name`) -- never fuzzy. The Hasheous dump links local contents by ROM hash to their exact IGDB/TGDB/RA/SGDB ids (`Confidence::Hash`, recorded in `hasheous_lookups` like a live lookup) and queues IGDB `provider_query` jobs for the IGDB ids found. Vendor additions (TGDB `platforms`/`games_by_platform_page`, EmuReady `list_games_page`) are in `playmatch_deredis/patches/{05,07}`. |
| fixes found running it | `database::lookup_or_insert_name`, `migrations/0003`, `import::is_imported`, `worker::dat_sync`, `playmatch_deredis/patches/01_igdb.patch` | (1) `sources` (TINYINT id) had run out of AUTO_INCREMENT: `INSERT IGNORE` on every `ensure_source` call burns an id even for a duplicate, so after ~255 calls a new source (`redump`, `no-intro`, `tosec`, `retrobios`) silently failed to insert and every import of it died "no rows returned". Lookup tables (`sources`, `genre`, `perspective`) now select first; `0003` resets their counters to MAX(id)+1. (2) `dat_sync` imported only freshly downloaded files, so files downloaded by an interrupted run (or by `test_database`, which shares the download dir) were never imported; it now also imports any file `is_imported` says has no `dat_imports` rows, and one bad DAT no longer aborts the rest (the job fails at the end, naming them). (3) IGDB list-by-ids requests had no `limit`, so IGDB returned its default 10 of each 100 ids; image `width`/`height` are optional (IGDB omits them on some covers). Both are vendor patches (also upstream bugs). |
| diff syncs, redownload | `ProviderBulkSyncRequest.mode` (`diff` default / `redownload`), `bulk::SyncMode`, `providers::sync_state`, `migrations/0004`, `0005` (`provider_dump_files`, `igdb_games.updated_at`) | A bulk sync fetches only what changed unless `redownload`: IGDB `updated_at > last complete sync - 1 day` (count first, for progress), TheGamesDB full dump by its recorded `Last-Modified` (its CDN only 304s an exact `If-Modified-Since` match and ignores weak ETags, so the date is sent alone and a 200 with the recorded validators is also treated as unchanged, the body never read; the schedule check uses a HEAD), Hasheous zips by md5, RetroAchievements per console by comparing the small `h=0` list with cached ids/achievement counts (the hash list only when something changed), EmuReady by `pagination.total`. By-id fetches skip entries younger than `cache_ttl_days` for every provider. |
| parallel Hasheous | `dumps::{sync_hasheous_dump, plan_hasheous_zips, download_if_changed}`, setting `hasheous_parallel_downloads` (1-16, default 4) | One `Dumps/platforms?includeDetails=true` call; changed zips download N at a time (JoinSet + semaphore) and are processed as each lands; an unchanged zip is re-processed from its local copy only when the catalog gained content since it was matched. Coverage per platform comes from `provider_dump_files`. |
| schedules | `database::schedules`, `api::schedules`, `providers::change_check`, `worker::run_due_schedules`, `GET/POST /schedules`, `GET/PUT/DELETE /schedules/{id}`, `POST /schedules/{id}/run`, Config > Schedules | A schedule is a job request (`provider_bulk_sync`, `dat_sync`, `db_optimize`) + mode + trigger `fixed` / `check` / `both`, each with its own every-N-unit (minutes..months, calendar months). A check asks the provider cheaply whether data changed (IGDB update count, TheGamesDB ETag, Hasheous md5s, EmuReady total; RA has none -> fixed only) and enqueues a diff only then. An identical pending/running job is never enqueued twice. |
| job cancel | `jobs::{cancel, cancel_requested, mark_cancelled}`, `POST /jobs/{id}/cancel`, worker `tick` | Pending -> `cancelled` at once; running -> `cancel_requested`, and the worker aborts the task on its next tick (~5 s), marks it cancelled and frees the lane. Retry accepts failed or cancelled jobs. |
| PlayMatch local-first + gap filler | `cache::{identify_local, record_playmatch}`, `playmatch::identify_bulk`, `api::scan::{playmatch_fill, playmatch_fill_candidates}`, job `playmatch_fill`, setting `playmatch_batch_size` | Hash match skips the online PlayMatch call when an exact local hash match exists and the IGDB id is known; other providers' ids are filled locally only from exact-hash sources (Hasheous dump mapping, RA md5 list, ScreenScraper rom index). The rest is bulk-identified (100 per request) in the background, only while no sync job is queued, and only for providers with usable credentials. `playmatch_batch_size`: N per batch, 0 = unlimited, -1 = off. |
| manual link: local search, URL paste | `api::links::{search_local, resolve_url}`, `GET /providers/{p}/search?local=true`, `POST /providers/resolve-url`, console match dialog | The match dialog searches a provider's locally cached catalog (every provider with a cache table, IGDB included; no network) or online, or takes a pasted provider game-page URL. Local hits show the provider's platform (TheGamesDB: its own platform, stored per cached game since `migrations/0007`; RetroAchievements: the synced console; IGDB: the cached game's platforms), and the platform filter narrows TheGamesDB / RetroAchievements hits. URLs: (IGDB `/games/<slug>` resolved through the API; TheGamesDB, RA, SteamGridDB, MobyGames, ScreenScraper, EmuReady, Hasheous by pattern). Links are `manual`, which nothing automatic replaces. |
| manual media | `api::media::{set_from_url, upload, remove}`, `POST /media/manual`, `POST /media/upload` (multipart, up to 512 MiB), `DELETE /media/{id}?title_id=` (or `release_id=`), console Media section on title/release pages | A user pick is its own `source = 'manual'` row (sharing the file when the bytes already exist), replaces the slot on the target and its revisions, is shown first, and a provider sync never adds to or replaces a slot holding one. Remove only unlinks; the row is retired once unlinked everywhere; no file is deleted. |
| media layout, cache, thumbnails | `database::{media_store, media_cache}`, `api::media::file`, `GET /media/{id}?size=thumb&format=`, job `media_relayout`, settings `media_cache_tier{1,2}_max_mb`, `media_thumb_{px,quality,format}` | See `docs/plan/database.md` "Media storage and cache". Grids request thumbnails. |
| RetroAchievements tab | `api::ra`, `providers::ra_account`, job `ra_account_sync` (lane `provider:retroachievements`, schedulable), `migrations/0008`, `GET /ra/status`, `POST /ra/refresh`, `GET /ra/games`, `GET /ra/games/{id}[?refresh=true]`, `GET /ra/games/{id}/image/{icon,title,ingame,boxart}`, `GET /ra/games/{id}/badges`, `GET /ra/badges/{name}[?locked=true]`, `GET /ra/avatar`, console tab RetroAchievements | RAChecker's profile/mastery view. Status block: avatar, name, hardcore/softcore/RetroPoints, local games, local games with an RA set, games played on RA, cached RA catalogue, award counts; the account is re-synced (profile + `API_GetUserCompletionProgress`, 500 per call) when older than an hour or on demand. Lists by highest award: Titles (all), Mastery (`mastered` = hardcore complete), Complete (`completed` = softcore), Beat (`beaten-*`), In progress (achievements earned, not mastered/completed; beaten included per `ra_include_beaten_default`), To be played (no entry or 0 earned). Titles is RetroAchievements' whole list of games with achievements (the cached RA catalogue) plus the account's and the local ones; To be played is Titles minus the other lists, never-started games included. The games pulldown shows local only, RetroAchievements' list only, or both local-first / RA-first (`ra_source_default`). A sets filter shows base sets, subsets (RA's "[Subset - ...]" games) or both. A base filter switches between console and genre (`ra_base_filter_default`): genre = RetroAchievements' own genre folded onto the 19 major genres, or a genre of a catalog title linked to the game. RA hands genres out one game at a time, so job `ra_genre_fetch` asks each game once (`API_GetGame`, which also fills its image paths; resumable): local games automatically after each account sync, the whole list on a button (~1.5 s per game). Views: list, cover grid, RA icon grid, RA title screen grid, RA screenshot grid, RA badges grid (a server-composed mosaic of the set's badges -- unlocked in colour, hardcore gold-framed, locked greyed -- shown square, cut at the bottom; whole on the game / title / release pages). A game page lists every achievement (filter all / achieved / unachieved / missable) from `API_GetGameInfoAndUserProgress`, fetched lazily and re-fetched when the account shows a newer unlock. RA images are downloaded once into `<image dir>/_retroachievements/` (the provider cache when no image dir is set; files already there move over when first needed); mosaics stay in the provider cache. `view_defaults` sets each list's default view (collection lists grid, RA lists badges). |
| merged releases, list speed | `database::release_merge`, `release_sources`, `catalog::paged` (`Lite`) | A release is one per platform + name; lists show all its DAT sources and filter by any, the release page groups files by dump (DAT source). Big lists page in two passes: count and page keys from a light query (aggregates only, grouped by primary key), then the display columns (cover, providers, verification counts -- per-row subqueries) for the page's rows only: ~6 s instead of ~60 s on 1.2M releases; filtered lists well under a second. |
| title merging, names, search | `database::title_merge`, `api::titles`, `POST /catalog/titles/merge`, `POST /catalog/titles/dedupe`, `GET /catalog/titles/merge-suggestions`, `POST /catalog/titles/merge-suggestions/{igdb_id}/{accept,dismiss}`, jobs `title_dedupe` / `igdb_reapply`, `migrations/0012`, `0013` | Titles split by regional names merge automatically when they share an IGDB id at exact / hash / manual confidence on a common platform (queued after IGDB-linking jobs, and at once on a manual IGDB link); other groups (name-search links, other platforms) go to the suggestions backlog; the user can also check titles in the list/grid and merge them. The survivor is renamed by region priority, keeps the others' names as aliases (a DAT re-import finds it), and every merge is logged. A file shared by more than 2 titles no longer spreads provider links (that had linked hundreds of unrelated games to one IGDB id). IGDB localizations and alternative names are stored (`title_locales`, `title_alt_names`), shown on the title page, and searched by the title search. Lists show a title by its top-priority release: its romanized DAT name, or its localized name per `title_display_mode`; releases show their localized name. |
| editing (Edit tab), naming | `api::titles`, `GET /catalog/keywords`, `PATCH`/`DELETE /catalog/keywords/{id}`, `POST /catalog/keywords/delete-unused`, `POST /catalog/titles/keywords` (+`/remove`), `DELETE /catalog/titles/{id}/keywords/{name}`, `GET /catalog/titles/merges`, `POST /catalog/titles/merges/{id}/decouple`, setting `canonical_name_priority`, `migrations/0014` | User keyword tags (source `user` in `title_keywords`): add / remove per title (title page, list/grid right-click, selection); Edit › Keyword tags lists keywords with their title counts, renames (onto an existing name joins them), deletes, and deletes all unused. Edit › Combined titles lists the merge log and decouples a merge: merges soft-delete the merged title and archive its rows (`title_merge_archive`), so it comes back under its old id with its releases and metadata; the pair is blocked from automatic re-merging (`title_merge_blocks`). A title's canonical name comes from its best commercial release by `canonical_name_priority` (default USA, EUR, WORLD, ASIA, en, JPN; independent of the display `region_priority`); the dedupe pass renames merged titles when that changes. TODO(release): editing is admin-only once accounts exist. |
| RetroAchievements patches | `database::ra_patches`, `providers::ra_hashes`, `POST /ra/patches/download` (`{md5}` → job `ra_patch_download`, lane `dat`), `GET /ra/games/{id}` (`releases[].ra_md5` / `patch_url` / `patch` / `patch_job`, `other_patches`), dir type `patch`, `migrations/0016` | The RA game page lists each supported release's RetroAchievements hash; a patched one gets **download patch** (the hash list comes from `API_GetGameHashes`, cached in `ra_game_hashes` and refreshed after `cache_ttl_days`). The archive is saved as is under `<patch dir>/<Platform variant>/` (only https from retroachievements.org / GitHub, 1 GiB cap, never overwriting a file the app didn't write), hashed with its members into `content` / `hashes` / `content_contents`, and recorded in `ra_patches` (patched release name, the stem its patch / sheet files share, whether they agree) and `ra_patch_files` (member kind: patch / sheet / readme / other). The same archive fetched for another hash is recognized by content and the new copy dropped. Directories of type `patch` are scanned and hashed but not made into ROMs. |
| archive contents, roms, contents, DAT resolve | `api::scan` (`hash_members`, `record_members`, `link_sheet_tracks`, `resolve_by_dat`), `database::content`, `GET /catalog/roms` (+`/{id}`), `GET /catalog/contents` (+`/{id}`), `GameRow.display_name` / `complete` / `contents_required` / `contents_present`, `migrations/0017` | Scan and rehash hash every archive's members (CHD tracks, zip / 7z / RVZ contents) straight from the decoder's stream -- nothing written to disk -- into `content` rows linked in `content_contents` with their path inside (`member_path`); a cue / gdi / ccd sheet's tracks are linked the same way. The DAT match counts member hits, so a CHD no mame-redump DAT lists still matches Redump / TOSEC by its tracks. Before matching, the DATs regroup the directory: loose roms covering different lines of one disc of a release become one rom (a container or sheet is a whole disc and never folds; copies of one dump stay apart), and roms of one release or of one "(Disc N)" set become one game, linked to each release, with disc numbers set. A game is complete when its files are present and some DAT source's dump of its release is fully held (container lines alone suffice; sheets aren't required when held as a container). Collection › Roms / Contents list them; a rom page lists its files (hover: members and hashes); a content page shows its canonical DAT name, hashes, files, parent archives, members and DAT releases. |
| selection, sets, hints, remembered lists | `POST /sets/{id}/releases` (`release_id` / `release_ids` / `title_ids` → `{added, set}`; a title adds its top-priority release, `sets::top_releases`), `GameRow.merge_hint` / `missing_found`, `RomRow.missing_found`, `FileRow.found_elsewhere`, console `makeGrid` | Titles and releases lists select by checkbox, ctrl/cmd+click on a row or cover, or ctrl/cmd+drag (every row / card the rectangle touches); selections add to a set (titles: their top-priority release) or, for titles, add / remove a keyword tag. 💡 marks a game whose incomplete local copies of one release would together hold a complete dump, and roms / games / files whose missing file's bytes another live file holds. Every list remembers its search, filters, sort and page for the browser tab's session, and each page its scroll position, so back / forward and returning through links land where you were. |
| RA-supported releases, counterparts, icons | `ra_account::LOCAL_RA`, `migrations/0013`, `database::counterparts`, `api::dev`, `GET /dev/counterparts/{full,stripped}.csv`, `romexplorer_dbtool export-counterparts`, `database::icon_import`, `romexplorer_dbtool import-icons` | RetroAchievements links also match RA's disc hash (`ra_hash`, not just file MD5); the RA tab counts only RA-supported releases (in RA's hash list, or linked by hand). The counterpart export: per platform, release -> USA / English counterpart (Japan-only releases name themselves, others without one are left empty for others to fill), as full and tag-stripped CSV. Platform icons: SVGs from the RomM submodule (read-only), with attribution. |
| art from the main server (the server as art cache) | `api::media_sync`, `sync::MEDIA_MIRRORED` (`GROUP_MEDIA`), `migrations/0029_media_mirror.sql`, `JobType::MediaSync` (`{"source_id": u16?}`, lane `sync:<source>`), `POST /sources/{id}/media`, `api::media::{refetch, fetch_for, provider_url_allowed}`, `POST /media/{id}/refetch`, `POST /media/fetch` (both `Read`), `media_cache::{cached_full, forget}`, settings `media_sync_after_pull` / `media_provider_fallback`, worker `media_download` via `media_sync::via_main`, GUI Sources "art", console `data-art` | The main server is the household's art cache, and a client never calls a provider while the server can be reached. A main server offers `media`, `title_media` and `release_media` alongside its catalogue (a share offers neither: the junctions are catalogue ids), mirrored under the source like ownership -- `media` is per machine, so the server's rows can't land in the client's own. `media_sync` walks the mirror: every server row whose title or release this machine has and whose slot is empty here (a local manual pick wins, as against a provider sync) is fetched through the server's `GET /media/{id}` with the source's own token -- which resolves through the server's cache tiers before its disk, so from the client the disk original is just the deepest tier -- and stored exactly as a download this machine made would be (same `source` / `provider_id` / `source_url`, same layout), so `relink_media` and the "already has this provider's cover" checks treat it as one; bytes or a URL this machine already holds are linked without a fetch. A 404 is a cache miss the server fills, not the client: `POST /media/{id}/refetch` restores the original from a cache tier if one still has it, else downloads it again from the row's own URL, then drops the tiers' stale copies; `GET /media/{id}` itself serves a cached full copy while the original is missing. A provider URL the server never had goes through `POST /media/fetch`, which fetches it into the server for the named title or release and hands back the row id -- so a `media_download` on a client (`via_main`) is served, refilled or newly cached by the server rather than fetched from the provider. Any signed-in account may fill the cache; what it may not do is point the server at an address (`provider_url_allowed`: a named host, never an IP literal or localhost, loopback only under the tests' `allow_public_ips`). The provider is called from a client only while the server is offline (`ping` unanswered), and only with `media_provider_fallback` on; a server that is up but cannot supply a file is that file's failure, never a reason to go around it. A pull that brought new art queues one `media_sync` (`enqueue_unless_queued`, `media_sync_after_pull`). Two-instance test in `test_api::media_from_the_server`. |
| audio library and page player | `api::audio`, `migrations/0034_audio_files.sql` (`audio_files`, `audio_file_tags`), `JobType::AudioScan` (`{"mode": full \| new_only}`, lane `audio`), `GET /audio`, `/audio/facets`, `/audio/tags`, `/audio/playlist?title_id= \| release_id= \| ra_game_id=`, `/audio/{id}/file` (Range), `POST /audio/tags`, `/audio/tags/remove`, `romexplorer_types::PLAYER_JS` (`/player.js`), GUI `screens::audio::{Audio, PagePlayer}`, console `showAudio` / `pagePlayer` | The folders with role *audio* are scanned recursively; tags are read with `lofty`. A file's keyword tags are stored by keyword **name** (the `keyword` table is replaced with the catalogue). A title, release or RA game page plays every playable track sharing a keyword tag with its title: shuffled, auto-started, stopped on leaving the page. DSD, ALAC and AIFF are converted on first play; WMA, APE and the like are listed and skipped (see the audio conversion row). One script draws the bar and the playlist panel in both UIs. |
| media settings, strip, keep-or-stream | settings `media_download` / `media_priority` / `media_show` / `media_client_keep` (`settings::MediaRules`, `SINGLE_MEDIA_TYPES`, `MEDIA_CAPABILITIES`; vocabulary key `media`), `providers::media::attach_media` (the gate), `catalog` `media_order!`, `GET /media/strip?title_id= \| release_id=` (`api::media::strip`), `GET /media/remote/{id}`, `GET /audio/remote/{id}/file` (`media_sync::from_main`, server `relay_from_main`, desktop `assets::relay`), `media_cache::{optimised_copy, store_thumb}`, `MediaSyncSummary.{streamed_only, converted}`, GUI `widgets::MediaStrip`, console `mediaStrip` | A provider sync downloads a media type only where `media_download` leaves it on, and for a one-per-game type skips a target that already has it from a provider ranked higher in `media_priority`; the same order (after a user's own pick) decides which of several is the cover. `MEDIA_CAPABILITIES` lists what each provider's sync fetches today, and the editors offer exactly that: IGDB box, screenshots, artworks (as `fanart`; its videos are YouTube links and are not fetched); SteamGridDB cover, heroes (as `fanart`), logo, icon; MobyGames box; RetroAchievements box, title screen, in-game shot (ScreenScraper waits on developer credentials). Only box and cover are on by default (`MEDIA_DEFAULT_ON`): the rest are opt-in, because for a whole catalogue they are tens of gigabytes and extra requests per game. List types go through `attach_media_list` (per file, by URL, at most `media_list_cap` = 6 per game and provider); `attach_igdb_extras` turns image ids into URLs in list requests of 100, only for titles with none yet. The strip on a title / release page shows every item of a type `media_show` allows (a title: its own and its releases', each file once); `bgm` off means no player. **Client mode:** `media_sync` keeps a copy only of the types `media_client_keep` ticks (video is off unless listed), storing images as AVIF when that is smaller, with the grid thumbnail made at that moment from the server's bytes because this build cannot decode AVIF. Everything else the server holds is listed in the strip as `remote` and streamed through `/media/remote/{id}`; a client's playlist also includes the server's tracks, streamed through `/audio/remote/{id}/file`. Requests to the server are sent `Accept-Encoding: identity` so a `Content-Range` describes the bytes that arrive. Audio is kept or streamed the same way, through `audio_sync` (next row but one). |
| audio conversion, DSD, kept copies, loudness | `deps/libxaac/{rxaac.c, rxaac.h, CMakeLists.txt, scripts/build.sh}` (the `rxaac` shared library; `build_native_deps.sh --only libxaac`, deployed by `move_libs_to_app_root.sh`), `rom::{xaac, m4a, audio_decode}`, `api::{audio_convert, audio_sync}`, `migrations/0035_audio_convert.sql` (`audio_files.support`, `loudness_lufs`, `remote_source_id`, `remote_id`), `GET /audio/{id}/file[?format=aac \| xhe_aac&bitrate=48-320]` (headers `X-Audio-Ext`, `X-Audio-Source`), `JobType::AudioSync` (no payload, lane `audio`), settings `audio_dsd_cache_format` / `audio_cache_max_mb` / `audio_client_format` / `audio_client_bitrate_kbps`, `bgm` in `media_client_keep`, crates `symphonia`, `rdsd2pcm` (git, GPL-3.0), `flac-codec`, `ebur128`, `romexplorer_types::PLAYER_JS` (volume, normalise) | **Support.** A file is `direct` (the browser plays it), `converted` (DSD, ALAC, AIFF: decodable here, not in a browser) or `unsupported` (WMA, APE, WavPack...: listed, never played or copied). **Conversion.** `symphonia` decodes, `rdsd2pcm` turns DSD into PCM, libxaac encodes AAC and xHE-AAC behind a flat C shim (its own API needs a struct pointer the published Rust binding leaves NULL, which crashes), `flac-codec` writes FLAC. A conversion is made whole, once, into `<media>/_audio_cache/` (`<id>-<what>-<source mtime>.<ext>`, least recently used removed past `audio_cache_max_mb`), then served with Range like any file; one conversion runs at a time. Without the encoder library AAC is not on offer: converted tracks are cached as FLAC and a client is sent originals. **Playing.** A `converted` track is served in the format `audio_dsd_cache_format` names (AAC 256 kbps, or FLAC: 24-bit/88.2 kHz for DSD, 16-bit for ALAC/AIFF because the decoder hands out 16-bit samples). **xHE-AAC** files list a frame a decoder can start at about once a second (`stss`), without which a player cannot seek, and declare a loudness of -16 LKFS, which the Windows, Apple and Android decoders take as \"leave the level alone\" (measured: declaring 0 played 16 dB quieter). **A client's kept copies.** With `bgm` ticked, `audio_sync` (button on Collection > Audio; queued after a catalogue sync from the main server) pages the server's `GET /audio` and fetches every tagged, supported track with `?format=&bitrate=` from its settings: the server converts a track above the bitrate (always, for a `converted` one) and sends the rest unchanged. Copies go under `_from_server/<album>/` and carry `remote_source_id` / `remote_id`. It is a sync: a held copy takes the server's current tags and details each run, and a copy whose track the server no longer offers is removed, file and row (only files under `_from_server`). The scan leaves these rows alone, and the playlist lists such a track once, as local. **Loudness.** `audio_scan` measures integrated loudness (EBU R128) for every decodable file that has none (new, changed, or scanned before this existed; DSD is not measured). The player's *normalise* scales each track's volume towards -16 LUFS, turning down only; it and the volume slider are remembered per machine. |
| ROM audio (music inside ROMs) | `deps/vgmstream/{rvgm.c, rvgm.h, CMakeLists.txt, scripts/build.sh}` (`rvgmstream`, vgmstream behind a flat C API; `build_native_deps.sh --only ffmpeg,vgmstream`), `deps/ffmpeg/scripts/build.sh` (static LGPL audio-only FFmpeg linked into rvgmstream; `vgmstream::has_ffmpeg`), `rom::{vgmstream, disc_files, rom_audio}`, `api::rom_audio`, `migrations/0079_rom_audio.sql` (`rom_audio_index`, `rom_audio_tracks`), `JobType::RomAudioIndex` (`{"mode": full \| new_only, "title_id"?}`, lane `rom_audio`), `GET /audio/rom/{id}/file` (Range), `/audio/remote/rom/{id}/file`, settings `audio_title_sources` / `audio_rom_kinds` / `audio_rom_not_owned` / `audio_keep_playing` / `audio_rom_{loop,bgm,jingle}_min_s`, `AudioRow.{rom, rom_kind}`, `Playlist.keep_playing`, `rxPlayer.leave` | Played from the ROM like a CD player: the index records where each track is (a CD audio track, or a file on the disc plus a subsong), never the audio. Disc filesystems are read in place (`disc_files`: ISO9660 in 2048/2352/2336 sectors, XDVDFS, GameCube FST; Wii is encrypted and refused) through CHD, cue/gdi, RVZ/WIA/GCZ, CSO/ZSO or the plain image; a ROM of many files is opened as its folder. vgmstream reads what it knows by extension, then content; a bank gives one track per subsong. Built in: ADX, HCA, DSP, PS-ADPCM, XA, ATRAC9, MPEG, the ADPCMs; through FFmpeg: WMA, XMA, ATRAC3/3plus, AAC, AC-3, Opus, Vorbis. Not yet: Wwise Vorbis (needs libvorbis), zip/7z archives, Wii. ISO9660 files flagged CD-XA form 2 (XA audio, STR video) are read as raw 2352-byte sectors, and a GD-ROM's high-density area (LBA 45000 on) is read as one filesystem across its data tracks. BGM / jingle / SE is worked out per playlist from loop, length and path words (`rom_audio::classify`), so the thresholds need no re-index; `kind_manual` overrides and survives a re-index. A track is decoded on first play (two loops and a 10 s fade) into `_audio_cache` as AAC 256 or FLAC (a rate AAC does not take, more than two channels, or `audio_dsd_cache_format = flac`), under the same cap. A title page plays its tagged audio and/or its ROMs' tracks; with none, `audio_rom_not_owned` takes another version from its Definitive family or a random title's, then `audio_fallback`. With `audio_keep_playing` the bar moves to a strip at the bottom when the page is left and plays until another title page opens. |
| link strength | `cache::LINK_UPSERT_KEEP_STRONGER` | A title/release provider link is only replaced by one at least as strong (manual > exact > hash > name > fuzzy), or when the old one was removed -- a later name/fuzzy pass can no longer overwrite a hash-exact id. |

Not yet: the worker's other handlers and dbtool still call `database` directly (steps 2-3).
No `remote_*` tables, `CatalogSync`, `CopyToLocal`, `PrepareForPickup`, or snapshot export.
The temp-dir picker isn't wired into `Single` yet (no job converts/extracts yet).
