# Database module v2: MariaDB schema, RAM/disk strategy, background services

## Context

 This plan
replaces the old 4-table hash-lookup-only design with the full schema the user actually
wants: files/content/rom/game/title/release/platform/provider tables, a durable per-provider metadata
cache (no Redis, matching the `playmatch_deredis` work), and background services (DAT
sync, throttled media downloads respecting provider rate limits, provider queries,
file scanning/hashing) that must keep making progress independent of whether the GUI/
browser is open -- the concrete motivation is a known failure mode in other solutions, where a scan
stops if the browser closes or the library is too large. `app_root_ex/lib/<os>/mariadb/` is
already assembled (mariadbd + libmariadb + headers from both build scripts);
`app_root_ex/assets/api.env` holds live provider credentials (IGDB, ScreenScraper,
SteamGridDB, TheGamesDB; MobyGames pending) and is correctly gitignored -- confirmed.

**Status: implemented (first pass) -- see "Implementation status" at the end of this
document for what landed, what the schema gained beyond this plan, and what's deferred.**

## Process architecture

**A separate background worker process, not in-process tokio tasks**, given the previous
failure mode: a long scan or a slow, rate-limited media-download queue must survive the
GUI closing, and ideally survive the main app process itself restarting. Shape:

- `romexplorer-worker` -- a second binary in this crate (or a sibling crate), started
  once (by the user, a service manager, or spawned-and-detached by the main app on first
  launch) and left running independently. It owns nothing in memory that matters --
  communicates with the main app **only through MariaDB**, via a `jobs` table (see
  below), using `SELECT ... FOR UPDATE SKIP LOCKED` to claim rows -- the standard SQL
  job-queue pattern, no custom IPC needed.
- The main app (GUI/API) only ever reads job status and enqueues new jobs by inserting
  rows. It never blocks on a job; it polls/subscribes to progress the same way whether
  it's a local GUI or a remote browser client, since it's just reading rows.
- This means progress survives: GUI closing (worker is a separate process), app crashes
  (worker unaffected), and even worker restarts (job state is in durable InnoDB rows, not
  in-memory) -- the worker just re-claims whatever's still `pending`/`in_progress` on
  startup.
- CPU-bound file scanning/hashing runs inside the worker on a dedicated thread pool
  (`native_lib::recommended_reader_threads`, already established), separate from the
  worker's own async I/O (provider queries, downloads) so one doesn't stall the other.

## RAM vs disk

**InnoDB on disk is the single source of truth for everything** -- durable, survives
crashes/power loss, which a RAM-primary design fundamentally can't guarantee. Two
complementary caching layers on top, not a competing "second database":

1. `innodb_buffer_pool_size` sized generously in `my.cnf` -- for a personal-scale catalog
   (thousands to low millions of rows) the working set plausibly fits entirely in the
   buffer pool after warm-up, giving RAM-speed reads with zero extra code.
2. A small in-process Rust cache (`moka`, already a dependency of the vendored playmatch
   provider clients) for the hottest, rarely-changing reference data the GUI touches
   constantly -- platform list, provider list, maybe a recently-viewed-games list.

`MEMORY`-engine tables are used only for genuinely ephemeral, restart-safe-to-lose state
(e.g. a live "currently scanning" progress counter for the GUI to poll) -- never for
anything the user would be upset to lose, which is everything else.

## Provider implementation order

Not a runtime/query behavior -- this is the order `provider_cache.rs` support for each
provider gets built and tested during implementation:

```
Hasheous > IGDB > ScreenScraper > SteamGridDB > EmuReady > MobyGames > TheGamesDB
```
(All 7 vendored via `playmatch_deredis`.)

Hasheous first because it's a hash-first lookup service -- it matches on the same
sha1/sha256/crc32/md5 identity this project's own `content`/`hashes` tables are built
around, so it's the most direct integration to stand up and verify end-to-end first. The
rest follow roughly by expected implementation complexity/payoff.

**RetroAchievements is a separate track, not part of the list above** -- it's a
from-scratch Rust client ported from [x3kim/RAChecker](https://github.com/x3kim/RAChecker)
(MIT; the user contributed the genre classification to their own fork of it), not a
`playmatch_deredis` vendor-and-patch job (see `retroachievements_games` under
Per-provider cache tables for what that involves). Reasonable to build any time
after Hasheous, since Hasheous can pre-populate its `release_provider_links` cheaply in
the meantime.

**A fourth track: playmatch's own matching logic, ported (not just its provider clients).**
The original motivation for playmatch was always its matching intelligence, not just raw
API access -- Redis was only ever a blocker for its *service* layer. Read through
`external/playmatch/service/src/matching/` directly (not assumed) to see what's actually
reusable there, beyond the 7 provider clients already vendored into `playmatch_deredis`:
- **Portable close to as-is** (pure functions, effectively no DB coupling):
  `name_parse.rs` (splits a DAT filename into base title + region/language/revision/disc/
  variant/year tags -- e.g. `"Example - Red Version (USA, Europe) (En,Fr,De,Es,It) (Rev A)"`
  into base title, `[Usa, Europe]`, `[En,Fr,De,Es,It]`, `Some("Rev A")`), `util.rs`
  (title normalization: diacritic folding, leading/trailing articles across many locales,
  roman numerals, trademark glyphs, ampersands -- depends only on `name_parse`), and
  `scoring.rs` (generic candidate gate-then-rank: hard-rejects on platform/year/variant
  mismatches, then scores year/region/variant agreement to pick a unique best match or
  flag ambiguity -- one small hook into playmatch's own metrics module, same shape as the
  no-op `metrics.rs` stand-in already used for the 7 vendored providers). These vendor
  much like the provider clients did: copy, patch the one metrics hook, done.

  **These two files aren't equally primary, though -- checked against the actual No-Intro
  DAT format (Logiqx), not assumed.** No-Intro DATs *do* carry a structured
  `<release name="..." region="..."/>` sub-element separate from the name-embedded tags,
  but real-world population of it is inconsistent (documented divergence between what's in
  the name and what's in that element) and it's not confirmed universal across other DAT
  families (Redump etc. -- needs checking per source, not assumed). So the priority is:
  - **Hash matches a DAT entry exactly** -- no candidate scoring at all, the release is
    already known. For populating `release.region`/`revision`/etc., prefer the DAT's own
    structured `<release>` fields when present; `name_parse.rs` is a *fallback* here, used
    only to backfill what the structured fields didn't carry -- not a matching mechanism
    on this path.
  - **No hash match, or reconciling against a name-search provider** (ScreenScraper/
    MobyGames/SteamGridDB `by_keyword` fallback scope, or a scanned file with no DAT hit)
    -- here `name_parse.rs`'s output is the actual matching input, and `scoring.rs`'s
    gate-and-rank is what it's really for: disambiguating multiple textually-similar
    candidates when hash identity isn't available at all.
  `util.rs`'s title normalization is used on both paths (indexed search on the hash-match
  path, candidate comparison on the no-hash-match path).
- **Concept worth porting, code needs rewriting** (deeply tied to playmatch's own
  Postgres/sea-orm schema -- `game`/`game_file`/`dat_file`/`signature_group`/
  `content_anchor` entities that don't map onto this project's tables): `content_anchor.rs`
  (assigns a hash-based anchor grouping unpopulated games get linked through) and
  `clone.rs` (links a "clone of" relationship between games sharing the same DAT-internal
  parent id within one `dat_file`) are DB-orchestration code, not pure algorithm -- not a
  patch-and-vendor job, but the underlying ideas are worth comparing against what this
  project already has before assuming a gap: `content_anchor` may already be covered by
  `content`/`content_contents`'s own hash-based dedup, and `clone.rs`'s parent/clone
  relationship looks closer to `release`-level DAT-variant grouping (a within-one-DAT-file
  "USA" vs "USA (Beta)" relationship) than to `title.basegame_title_id` (which is about
  ports/remakes, a different axis) -- needs a closer look at exactly what
  `signature_group_internal_clone_of_id` groups before deciding whether it's redundant or
  a genuine gap. `manual.rs`/`suggestions.rs` (per `matching/mod.rs`'s own doc comment: an
  operator-approval workflow for proposed matches) are likely the same story, not yet read
  in full.
- Lands as a new module once implemented (`src/database/matching.rs` or similar) --
  `name_parse`/`util`/`scoring` ported near-as-is, used by `import.rs`/`provider_cache.rs`
  when reconciling a DAT/scanned name against release/title candidates instead of writing
  that matching logic from scratch.

**Runtime priority (which provider's data wins on conflict, query order for a new match)
is a configurable setting, not a hardcoded order.** It needs a place to live -- e.g. a
`settings`/`config` table (or a column on an existing config row) holding an ordered list
the user can rearrange, which `provider_cache.rs`'s conflict-resolution and the
background query scheduler both read at runtime instead of a compiled-in constant. Exact
storage shape (JSON array in a `settings` table vs. a dedicated
`provider_priority(provider, rank)` table) not decided yet -- flagged as an open point,
default ordering can just be the implementation order above until the settings mechanism
itself is designed.

**Per-provider sync strategy is the same kind of setting, same undecided storage
mechanism.** RAChecker's real, working RA sync is the model: `API_GetGameList` per
console bulk-downloads every game+hash for that whole system in one call (no per-game
fetches), cached with a TTL (90 days there), and a *separate* on-demand pass
(`enrichHashNames`/`enrichGenres`) fills in per-game detail the bulk list doesn't carry --
bulk and on-demand aren't alternatives, they're complementary layers.

Two independent things, not one flat mode:
- **scope** -- what's actually being asked for: `single` (one item, by id/hash), `list`
  (a batch of ids/hashes in one request -- e.g. IGDB's Apicalypse `where id = (1,2,3,...)`,
  and Hasheous already has a confirmed `POST /Lookup/ByHash` batch endpoint distinct from
  the single-hash `GET` used earlier), `full` (the provider's entire database),
  `by_platform` (bulk, scoped to platforms present in the user's collection -- the
  practical default for a huge catalog like IGDB, where `full` is wasteful but
  per-platform pagination is well-supported), or `by_keyword` (a text search, for
  providers whose natural match path is name-search rather than id/hash -- e.g. a
  SteamGridDB/MobyGames fallback when hash-matching comes up empty). Which scopes a given
  provider actually supports is provider-dependent and needs verifying at implementation
  time (confirmed true for RA: `single` and `by_platform`; confirmed true for Hasheous:
  `single` and `list`; not assumed for the rest).
- **trigger** -- *why* a job got enqueued: a background scheduler tick (proactive), or
  the app reacting to something the user just did (e.g. browsing a not-yet-synced
  platform's library). This isn't a stored property of the job itself -- the same
  `by_platform` sync can be enqueued either way, so it doesn't need its own column, just
  whatever code path decides to insert the row.

`list` exists specifically to cut request count on providers with tight rate limits
(MobyGames' free tier, ScreenScraper's daily caps) and plugs into something the worker
already does: its claim query (`... ORDER BY id LIMIT ? FOR UPDATE SKIP LOCKED`, under
`jobs` below) already pulls a *batch* of pending jobs at once. When several claimed
`provider_query` jobs target the same provider and that provider supports `list`, the
worker coalesces them into one request instead of N, then fans the single response back
out to mark each job done individually -- not a new mechanism, just using the batch
already being claimed.

Maps onto the `jobs` table as two job types by scope, not by trigger: `provider_query`
(scope `single`/`list`/`by_keyword`) and `provider_bulk_sync` (scope `full`/`by_platform`,
payload carries the provider and, for `by_platform`, which platform(s)). Either can be
enqueued proactively or reactively. `provider_query`-style enrichment still runs on top of
a `provider_bulk_sync` result regardless of scope (RA's own enrichment pass happens after
its bulk sync, not instead of it). Staleness/re-sync timing reuses `fetched_at` on the
relevant cache table plus a configurable TTL, same idea as RAChecker's 90 days.

## Schema

`sources` and the old `games`/`local_files`/`hashes` shape are superseded; kept only as the
`hashes` table's own design (surrogate ID + (algorithm, value_hex) index) carries forward.

**Deletion policy.** Default behavior on every entity table below is soft-delete: set
`deleted = TRUE`, never `DELETE FROM ...`. Actual row removal only happens in a periodic
maintenance pass (`db_optimize`, a new `jobs.job_type`, see the `jobs` table) that walks
the dependency graph bottom-up and hard-deletes a row only once it's flagged `deleted` and
nothing still live depends on it:
- `content`/`local_files`/`local_rom`/`local_game`: purged only once confirmed no on-disk file in the
  collection is still tied to that entry (i.e. `files.deleted` true for every file the
  chain resolves to -- a `content` row isn't purged while any non-deleted `local_files` row
  still points at it via `content_id` or a live `content_contents` parent, a `local_rom` isn't
  purged while any non-deleted `local_rom_files` row still points at it, a `local_game` isn't purged
  while any non-deleted `local_game_roms` row still points at it).
- `release`/`title`: not tied to on-disk files directly, so purged instead once no live
  (`deleted = FALSE`) `local_game`/`release` respectively still links to them (via
  `local_release_games`/`title_releases`).
This means a temporarily-absent drive (files just not found at last scan) never loses
match history or provider links -- only a maintenance pass, run explicitly, ever removes
rows, and only once nothing on disk still needs them.

Applies to every entity table with its own identity (`content`, `hashes` excepted --
see below --, `local_files`, `local_rom`, `local_game`, `release`, `title`, `genre`, `franchise`, `company`,
`media`, the per-provider cache tables,
`title_provider_links`/`release_provider_links`). Excluded, with reasons: pure structural
junctions (`content_contents`, `local_rom_files`, `local_rom_rahash`, `local_game_roms`, `local_release_games`,
`title_releases`, `title_genres`, `title_franchises`, `title_companies`, `title_media`,
`release_media`, `screenscraper_rom_index`) have no independent meaning beyond the current
relationship, so a structure change is just a row insert/delete, not a soft-delete;
`hashes` and `content_platform_data` rows have no lifecycle independent of their `content`
row's; `emuready_reports` refreshes in place (a stale report is just re-fetched, not
soft-deleted); `jobs` already models lifecycle via `status`; `provider_quota`/`sources` are
small, effectively-static operational/reference tables, not collection entries; `platforms`
is fixed, app-defined reference data (see below), not a collection entry either.

Ordered low-level (raw bytes) to high-level (catalog/display) below: `content` ->
`content_platform_data` -> `content_contents` -> `hashes` -> `local_files` -> `local_rom` ->
`local_rom_files` -> `local_rom_rahash` -> `local_game` -> `local_game_roms` -> `release` -> `local_release_games` ->
`title` -> `title_releases` -> `genre`/`franchise`/`company` (+ their `title_*`
junctions) -> `platforms` -> `media` (+ `title_media`/`release_media`). Per-provider
cache tables and their `*_provider_links` junctions attach at whichever of
`content`/`release`/`title` each provider naturally keys at -- see that section for
specifics.
Each of `local_game`/`release`/`title` owns a trailing junction to the table below it
(`local_game_roms`, `local_release_games`, `title_releases`) instead of a plain FK column on the
child -- uniform shape for "list children of X" regardless of cardinality, matching
`local_rom_files`/`content_contents`.

### `content`
The deduped, hash-identified payload -- what a ROM's bytes actually *are*, independent of
which archive/file currently wraps them.
```
id               BIGINT UNSIGNED PK AUTO_INCREMENT
deleted          BOOLEAN NOT NULL DEFAULT FALSE  -- true once every file/content_contents parent pointing here is itself deleted
app_platform_id  SMALLINT UNSIGNED NULL      -- common::models::Platform's own id -- not IGDB's; see `platforms` table
file_name        STRING -- Filename conventional to redump/nointro database. actual/internal path/extracted filenames will depend on parent.
size              BIGINT UNSIGNED NOT NULL
headered          BOOLEAN NULL                -- RomMetadata::headered -- intrinsic to these exact bytes
encrypted         BOOLEAN NULL                -- RomMetadata::encrypted
byte_order        ENUM('big','byte_swapped','little') NULL  -- RomByteOrder, N64-relevant
media_type        VARCHAR(32) NULL            -- common::models::MediaType
partial           BOOLEAN-- if it is a partial file(bin/raw etc)
first_seen_at     DATETIME NOT NULL
```
Headered/encrypted/byte-order live here, not on `platforms` (platform is classification
only, not per-instance state) and not duplicated per-`local_rom` (two byte-identical files are
the same content and share this data by definition).

**Dedup rule**: two files are the same `content` row only when they match on the
*strongest* hash both have available (prefer sha256, fall back to sha1) -- never dedup on
crc32 alone, which has meaningfully higher collision odds at this data scale.

### `content_platform_data`
Platform-specific technical metadata needed to correctly interpret/convert a ROM's bytes,
beyond `content`'s own `headered`/`encrypted`/`byte_order` -- e.g. which decryption scheme
and key a 3DS/PSP title needs. Deliberately not more sparse nullable columns on `content`
(most platforms need none of this, and `content` is filtered/sorted constantly, so keeping
it lean matters) and deliberately not one small table per platform (an ongoing-migration
burden every time a new platform needs one field) -- a JSON side table instead, same
"full fidelity, avoid modeling every field as a column" precedent as the provider
response caches further down (this one's our own generated data though, always JSON, not
a third-party response, so no format-on-disk question the way those have).
```
content_id  BIGINT UNSIGNED PK FK -> content(id)  -- 1:1, row only exists when there's platform-specific data
data        JSON NOT NULL
updated_at  DATETIME NOT NULL
```

### `content_contents` (junction, self-referencing on `content`)
What a piece of content decodes/extracts into, when it's a container (an archive holding
several ROMs; a chd wrapping raw tracks) rather than a leaf payload. Modeled on `content`,
not on `local_files`: the containment relationship belongs to *these exact bytes*, so it's
computed and stored once no matter how many file paths happen to be byte-identical copies
of the same archive. Self-referencing so nested containers (a zip holding a 7z holding
ROMs) fall out for free, no special-casing.
```
parent_content_id  BIGINT UNSIGNED NOT NULL FK -> content(id)
child_content_id   BIGINT UNSIGNED NOT NULL FK -> content(id)
PRIMARY KEY (parent_content_id, child_content_id)
```
A leaf piece of content (the common case, nothing further to extract) simply has no rows
here as a parent; this table exists only for the container case.

### `hashes`
```
id          BIGINT UNSIGNED PK AUTO_INCREMENT
content_id  BIGINT UNSIGNED NULL FK -> content(id)  -- NULL for rom-level hashes, see rom_rahash below
algorithm   ENUM('crc32','md5','sha1','sha256','xxh3_128','ra_hash') NOT NULL
value_hex   VARCHAR(64) NOT NULL
UNIQUE KEY (content_id, algorithm)
INDEX (algorithm, value_hex)   -- the actual hash -> content lookup path
```
Surrogate `content.id`, not the hash itself, as the join target for the content-derived
algorithms -- a hash column as PK breaks the moment an entry is missing that particular
algorithm (common; sources vary in which hashes they publish), and an 8-byte BIGINT is far
cheaper to carry into every downstream FK/index than a 40-64 byte hex string repeated
everywhere. `hashes` itself now also has a surrogate `id`, since `ra_hash` rows (below)
need to be pointed at directly rather than reached through `content_id`.

### `local_files`
Physical files on disk. Every file's own raw bytes are hash-identified content too --
including archives -- so `content_id` is always set, never NULL: for a plain ROM file it
points at the same content the payload *is*; for an archive it points at the archive's
own bytes as a whole (its own hash), letting two byte-identical copies of the same zip
dedup exactly like two copies of the same ROM, and giving a cheap whole-file integrity
check without re-extracting. What an archive's bytes *decode into* is a separate concern,
see `content_contents` above.
```
id                     BIGINT UNSIGNED PK AUTO_INCREMENT
path                   VARCHAR(1024) NOT NULL
content_id             BIGINT UNSIGNED NOT NULL FK -> content(id)
archive_format         VARCHAR(16) NULL        -- common::models::ArchiveFormat
size                   BIGINT UNSIGNED NOT NULL
deleted                BOOLEAN NOT NULL DEFAULT FALSE  -- no longer found on disk at last scan
last_modified          DATETIME NULL
last_integrity_check   DATETIME NULL
last_full_check        DATETIME NULL
INDEX (content_id)
```
Mirrors `common::models::FileData` closely on purpose -- this table is that struct's
persisted form.

### `local_rom`
Groups the file(s) that make up one playable unit -- a single ROM file, or a
cue/gdi+bin/raw set, or a directory-based dump. No `game_id` column -- that relationship
is now the `local_game_roms` junction, below `local_game`.
```
id               BIGINT UNSIGNED PK AUTO_INCREMENT
app_platform_id  SMALLINT UNSIGNED NULL   -- as-detected: scan heuristic first, DAT hash-match confirms/overwrites
disc_number      SMALLINT UNSIGNED NULL               -- ordering within a multi-disc game
main_path        VARCHAR(1024) NOT NULL               -- the file/dir/cue path representing this rom
alt              BOOLEAN NOT NULL DEFAULT FALSE        -- an alternate dump/mastering of the same disc (No-Intro/Redump "(Alt)")
deleted          BOOLEAN NOT NULL DEFAULT FALSE        -- true once its constituent files are gone
INDEX (app_platform_id)
```
### `local_rom_files` (junction)
```
rom_id   BIGINT UNSIGNED NOT NULL FK -> local_rom(id)
file_id  BIGINT UNSIGNED NOT NULL FK -> local_files(id)
PRIMARY KEY (rom_id, file_id)
```

### `local_rom_rahash` (junction)
RetroAchievements' hash is computed per playable unit, not per deduped payload -- for some
platforms it spans more than one file (e.g. a disc's boot executable combined with region
data), so unlike crc32/md5/sha1/sha256/xxh3_128 it can't attach via `hashes.content_id`.
Points at a `hashes` row with `algorithm = 'ra_hash'` and `content_id` left NULL.
```
rom_id   BIGINT UNSIGNED NOT NULL FK -> local_rom(id)
hash_id  BIGINT UNSIGNED NOT NULL FK -> hashes(id)
PRIMARY KEY (rom_id, hash_id)
```

### `local_game`
Top-level: one real-world game, one or more `local_rom`s (via the `local_game_roms` junction below),
optionally an m3u playlist file. **No separate `playlist` table** -- resolves the "combine
rom-grouping and playlist tables?" question: keeping them separate forces every game into
exactly "1 rom XOR 1 playlist", which has no slot for a playlist-less multi-disc game
(multiple roms, no m3u) without inventing a synthetic playlist row anyway. `local_game`
referencing `Vec<rom>` plus an optional playlist file naturally covers all three shapes:
single rom, multi-disc with m3u, multi-disc without m3u.

**A `local_game` row is always exactly one physical file-set, complete or not; rows are never
merged in storage.** Two separate attempts at "Super Hoge" (one owning disc 1 + disc 2,
another owning a duplicate disc 1 with disc 2 missing) are two permanent, separate `local_game`
rows, even though a human would call them "the same game." A file belongs to at most one
`local_rom`, a `local_rom` to at most one `local_game` -- no two `local_game` rows ever share a file. Deduping or
grouping near-identical or region-variant copies for display is a UI/query-time concern
only, never collapsed at the schema level.
```
id                BIGINT UNSIGNED PK AUTO_INCREMENT
name              VARCHAR(512) NOT NULL
playlist_file_id  BIGINT UNSIGNED NULL FK -> local_files(id)  -- the .m3u, when one exists
region            VARCHAR(128) NULL
category          VARCHAR(64) NULL
cover_file_id     BIGINT UNSIGNED NULL FK -> media(id)
deleted           BOOLEAN NOT NULL DEFAULT FALSE  -- true once no constituent rom/file remains
INDEX (name)
```
No `app_platform_id` here -- see below.

**Three-tier naming (resolved):** `title` (abstract, region-independent) -> `release`
(DAT-level, region/revision-specific catalog record) -> `local_game` (this table -- a specific
owned file-set, ownership data). `local_game`'s own `name`/`region`/`category` remain as
as-scanned/fallback values (useful before anything is matched, same role `content.file_name`
plays relative to catalog data); once a `local_game` appears in `local_release_games` below, the
catalog-canonical `title`/`release` name and region are the ones surfaced for display,
without needing to overwrite what was actually scanned.

**Platform doesn't belong on `local_game` (or `title`, below) at all.** A simultaneous
multi-platform launch (same title, same release date, one `release` per platform) is a
real, common case -- a `title`/`local_game` pinned to a single platform would be wrong for it,
and true ports (SFC -> GBA) are already handled correctly as *separate* `title` rows
(`category='port'`), not by this column. Platform instead lives at two places with
different purposes: `rom.app_platform_id` (below `local_game_roms`) is the earliest-available
guess -- filled from local scan heuristics (extension/header sniffing) the moment a file
is found, then confirmed/overwritten once a DAT hash-match resolves it, since a DAT is
itself always platform-scoped. `release.app_platform_id` (below) is the catalog-canonical
value, from whichever platform-scoped DAT the release was imported from, or from a
provider's own reported platform. `content.app_platform_id` (already existed, kept
as-is) is a third, distinct value: intrinsic to the hash-verified payload itself, once
that payload is confirmed via hash-match -- not a duplicate of `local_rom`'s early guess, since
`content` only gets a value once hashing/dedup has actually happened. `local_game`/`title`
resolve platform by joining down to these rather than caching it themselves. All three are
the app's own platform identity (`common::models::Platform`), not IGDB's -- see
`platforms` below for how the two map.

### `local_game_roms` (junction)
Replaces the old `rom.game_id` column. A game matches one release as a whole (see
`local_release_games` below) rather than each disc/rom independently -- the roms beneath a game
are just its file grouping, not separately re-matched.
```
game_id  BIGINT UNSIGNED NOT NULL FK -> local_game(id)
rom_id   BIGINT UNSIGNED NOT NULL FK -> local_rom(id)
PRIMARY KEY (game_id, rom_id)
```

### `release`
The DAT-level, region/revision-specific catalog record for one shipped SKU as a whole --
**not literally "one DAT line"**: No-Intro DAT entries (mostly single-file games) map ~1:1
onto a `release`, but Redump splits multi-disc games into one DAT line *per disc* (e.g.
"(Disc 1)"/"(Disc 2)"/"(Disc 3)" are three separate entries). Those roll up into a single
`release` at import time once recognized as the same set (by naming convention or
Redump's own disc-set metadata) -- each disc's hash is still matched individually against
its own DAT line at the `local_rom`/`local_files` level; `release` is the higher-level "this SKU,
however many discs it took" grouping on top of that. Reference/catalog data, matched onto
a `local_game` -- not itself something the user owns; a `release` can exist in the catalog with
zero matching `local_game`s. No `title_id` column -- that relationship is the `title_releases`
junction, below `title`.
```
id                 BIGINT UNSIGNED PK AUTO_INCREMENT
app_platform_id    SMALLINT UNSIGNED NULL  -- catalog-canonical: from the platform-scoped DAT it was imported
                                            -- from, or a provider's own reported platform
name               VARCHAR(512) NOT NULL   -- as given by the source DAT/provider, region/revision tags intact
region             VARCHAR(128) NULL
revision           VARCHAR(32) NULL
released_on        DATE NULL                -- when this specific regional edition shipped
source_id          TINYINT UNSIGNED NULL FK -> sources(id)
is_beta            BOOLEAN NOT NULL DEFAULT FALSE
is_prototype       BOOLEAN NOT NULL DEFAULT FALSE
is_demo            BOOLEAN NOT NULL DEFAULT FALSE
community_patched  BOOLEAN NOT NULL DEFAULT FALSE
patch_type         ENUM('gameplay','translation') NULL  -- classification when community_patched is true
deleted            BOOLEAN NOT NULL DEFAULT FALSE  -- true once no live game still links here
INDEX (name), INDEX (app_platform_id)
```

### `local_release_games` (junction)
Replaces the old `rom.release_id` column -- moves the match from per-disc up to per-game
(one owned attempt matches one release as a whole).
```
release_id  BIGINT UNSIGNED NOT NULL FK -> release(id)
game_id     BIGINT UNSIGNED NOT NULL FK -> local_game(id)
PRIMARY KEY (release_id, game_id)
```

### `title`
Abstract, region-independent identity -- "Super Hoge" as one thing regardless of which
release (JP/US/EU, any revision) realizes it, **as long as the actual gameplay content is
the same**. Grouping is on content, not name/region: "Super Hoge (JPN)" and "Super Hoge
International (JPN)"/"Super Hoge (US)" are *separate* `title` rows whenever the game
content genuinely differs (censorship, added/removed content, a region-bundle release),
not just when the box/name differs. Ports and remakes are always separate `title` rows
too -- never merged into the original -- linked instead via `basegame_title_id`, mirroring
IGDB's own category/parent-game split. Reference/catalog data, not ownership -- never
linked to directly from `local_files`; every owned `local_game` reaches it through `local_release_games`
above and then `title_releases` below.
```
id                 BIGINT UNSIGNED PK AUTO_INCREMENT
name               VARCHAR(512) NOT NULL   -- canonical, region-independent name
category           ENUM('base_game','port','remake','remaster','dlc','expansion','bundle','mod')
                    NOT NULL DEFAULT 'base_game'   -- mirrors IGDB's Game.category
basegame_title_id  BIGINT UNSIGNED NULL FK -> title(id)  -- NULL for base_game; otherwise the
                                                          -- title this one derives from
deleted            BOOLEAN NOT NULL DEFAULT FALSE  -- true once no live release still links here
INDEX (name), INDEX (basegame_title_id)
```
`basegame_title_id` is a plain self-referencing column, not a junction -- unlike
genre/franchise/company below, a title needs exactly *one* deterministic base game to
group/sort ports, remakes, and remasters under (a junction would allow an ambiguous
many-to-many here, which isn't a real case worth the complexity). Chains work naturally
(a port of a remake still resolves back to the original by following the pointer).

### `title_releases` (junction)
Replaces the old `release.title_id` column.
```
title_id    BIGINT UNSIGNED NOT NULL FK -> title(id)
release_id  BIGINT UNSIGNED NOT NULL FK -> release(id)
PRIMARY KEY (title_id, release_id)
```

### `genre` / `title_genres` (junction), `franchise` / `title_franchises` (junction), `company` / `title_companies` (junction)
Genre, franchise, and company (developer/publisher) are genuinely many-to-many (a title
commonly has several genres, occasionally spans more than one franchise, and often has
separate developer/publisher/porting-studio companies) -- real junction tables, not a
column on `title`, so they stay in 1NF and index/filter/sort fast in the GUI without going
back to the provider's raw response file at query time. Populated by extracting these out
of a provider's cached response at write time (`provider_cache.rs`), not reinvented -- the
raw response file (see Per-provider cache tables, below) remains the full-fidelity
fallback.
```
genre              id SMALLINT UNSIGNED PK, name VARCHAR(64) UNIQUE NOT NULL, deleted BOOLEAN NOT NULL DEFAULT FALSE
franchise          id BIGINT UNSIGNED PK AUTO_INCREMENT, name VARCHAR(256) UNIQUE NOT NULL, deleted BOOLEAN NOT NULL DEFAULT FALSE
company            id BIGINT UNSIGNED PK AUTO_INCREMENT, name VARCHAR(256) NOT NULL, deleted BOOLEAN NOT NULL DEFAULT FALSE

title_genres       title_id BIGINT UNSIGNED NOT NULL FK -> title(id), genre_id SMALLINT UNSIGNED NOT NULL FK -> genre(id), PRIMARY KEY (title_id, genre_id)
title_franchises   title_id BIGINT UNSIGNED NOT NULL FK -> title(id), franchise_id BIGINT UNSIGNED NOT NULL FK -> franchise(id), PRIMARY KEY (title_id, franchise_id)
title_companies    title_id BIGINT UNSIGNED NOT NULL FK -> title(id), company_id BIGINT UNSIGNED NOT NULL FK -> company(id),
                    role ENUM('developer','publisher','porting_studio') NOT NULL, PRIMARY KEY (title_id, company_id, role)
```
`genre`/`franchise`/`company` get `deleted` (real reference entities); the three junctions
don't, same reasoning as `content_contents`/`local_rom_files`/etc -- a link either exists or is
replaced, no independent lifecycle to soft-delete.

### `platforms`
Reference/classification data only -- not per-instance state (that's on `content`). PK is
the app's own platform identity (`common::models::Platform`), not IGDB's -- the two aren't
guaranteed to match one-to-one (the app may recognize platforms IGDB doesn't, or split/
group differently), so `igdb_id` here is a separate, nullable cross-reference column used
only when querying IGDB by platform, not the join target every other table uses.
```
id            SMALLINT UNSIGNED PK   -- common::models::Platform's own id
igdb_id       SMALLINT UNSIGNED NULL UNIQUE  -- cross-reference for querying IGDB; NULL if IGDB has no equivalent
name          VARCHAR(128) NOT NULL
family        VARCHAR(32) NULL       -- nintendo/sega/sony/... , mirrors platforms::helpers.rs classifiers
is_disc_based BOOLEAN NOT NULL DEFAULT FALSE
```
No `deleted` -- this table is fixed, app-defined reference data (a compiled-in enum), not
something that gets deleted through normal operation; it only changes when the app itself
is updated, or via manual repair.

### `media`
Cover art, box scans, screenshots, video, BGM -- files on disk. **Not owned by `local_game`** --
box art, screenshots etc. are catalog data (the same image applies to every owned copy of
a release, or every release under a title), so re-attaching it per owned `local_game` would mean
re-downloading/duplicating the same file for every duplicate copy a user happens to have.
```
id            BIGINT UNSIGNED PK AUTO_INCREMENT
media_type    ENUM('cover','box','box_3d','title_screen','screenshot','video','bgm','fanart','logo') NOT NULL
sub_category  ENUM('front','back','top','bottom','left','right','spine') NULL  -- angle/side,
                                                                                -- only meaningful for box/box_3d
path          VARCHAR(1024) NOT NULL
source        VARCHAR(32) NULL   -- which provider it came from
deleted       BOOLEAN NOT NULL DEFAULT FALSE
INDEX (media_type, sub_category)
```
`game.cover_file_id` (above) still picks *which* media row to show for that owned copy --
it's just a pointer now, not the attachment point.

### `title_media` / `release_media` / `franchise_media` / `keyword_media` (junctions)
Real junction tables, not a single FK column, for two reasons: a title/release can have
many media rows (multiple box angles, several screenshots), and the same media row can
legitimately apply to more than one release (near-identical regional editions sharing
literally the same box scan shouldn't mean downloading/storing it twice). Whether the GUI
pools all of a title's media together across platforms/regions or shows only the exact
release's own media is a display-time config/sort choice, not a schema constraint -- query
`release_media` for the exact-release view, `title_media` for the pooled view.

**Attachment rule (decided):** images and videos belong to *releases*, even when the
provider's entity is title-level -- an IGDB/SteamGridDB/MobyGames cover is downloaded once
and linked to every release of the title, and region-specific art from release-tier
providers (RetroAchievements box art, later ScreenScraper per-region media) sits next to it
on its own release. A title's grid art is therefore its *best-ranked release's* art (see
`ReleaseRank` in `api::catalog`: RetroAchievements-compatible first, then `region_priority`,
then `language_priority`). Audio belongs to a title (its soundtrack), or -- for arrangement
tracks spanning games -- to a franchise or a **keyword** (`keyword`, `title_keywords`
(source-tagged: user or a provider's keywords), `keyword_media`), e.g. one keyword covering
an original and its remake. `title_media` remains for title-level audio and as the fallback
for art nothing else provides.
```
title_id    BIGINT UNSIGNED NOT NULL FK -> title(id)
media_id    BIGINT UNSIGNED NOT NULL FK -> media(id)
PRIMARY KEY (title_id, media_id)

release_id  BIGINT UNSIGNED NOT NULL FK -> release(id)
media_id    BIGINT UNSIGNED NOT NULL FK -> media(id)
PRIMARY KEY (release_id, media_id)
```

### Per-provider cache tables (one per vendored `playmatch_deredis` provider, plus `retroachievements_games`)
Checked before ever making a live call (cache hit = no request; miss or explicit
force-refresh = query via the vendored client). Providers don't all model "a game" at the
same granularity though, so these split into three attachment patterns, not one shape.
`retroachievements_games` is the one exception to "per vendored provider" -- it's live
too, but a from-scratch Rust client ported from a reference implementation rather than a
`playmatch_deredis` vendor-and-patch job (see Release-keyed, below, for why).

Raw responses are cached **on disk, as whatever format the provider actually returns**
(JSON, protobuf, other) -- not forced into a JSON column. Same "media on disk, metadata in
DB" precedent as `media` above: avoids assuming JSON when a provider doesn't use it,
avoids base64-bloating a binary response into a text column, keeps table/row size (and so
the InnoDB buffer pool's effectiveness) independent of response size, and the raw file is
directly inspectable for debugging without DB tooling. Every cache table below carries
`raw_response_path`/`raw_format` instead of `raw_response JSON`, under a convention
mirroring the existing DAT download layout (`app_root_ex/assets/cache/provider_responses/
<provider>/<key>.<ext>`).

**Whether the file is actually kept is a config setting, `raw_response_retention`** (same
undecided settings mechanism as provider priority/sync strategy above), not a fixed
behavior:
- `keep_all` -- every fetch's raw response is saved as its own file (versioned/timestamped)
  -- full history, useful if extraction logic (`provider_cache.rs`) improves later and old
  responses are worth reprocessing; costs the most disk.
- `keep_latest` -- only the most recent raw response is kept, overwritten on each re-fetch
  -- still inspectable, bounded disk use.
- `discard` -- parsed and validated in memory at fetch time, then thrown away; only the
  structured cache-table row persists. Cheapest, but forgoes ever reprocessing a response
  after the fact (e.g. to backfill a field the parser didn't originally extract).
`raw_response_path`/`raw_format` are therefore **nullable**, not `NOT NULL` -- both NULL
under `discard`. The two things that need to stay consistent when a file *is* kept (file +
row) are written file-first, row-second, and cleaned up together in the same `db_optimize`
pass that hard-deletes anything else -- not two independently-managed lifecycles.

**Hash-keyed** (`hasheous_lookups`, `playmatch_lookups`) -- Hasheous and the hosted PlayMatch
service are pure hash -> identity lookup services, so their caches are keyed directly by
`content_id`, not a provider-assigned id, and need no `*_provider_links` junction -- the key
already *is* the match. What they *report* (the game's ids at IGDB/RA/...) is written into
the links with `confidence = 'hash'`. `playmatch_lookups` adds `match_type` (its
gameMatchType: SHA256/SHA1/MD5/CRC/FileNameAndSize).
```
content_id         BIGINT UNSIGNED PK FK -> content(id)
raw_response_path  VARCHAR(1024) NULL  -- NULL under raw_response_retention='discard'
raw_format         VARCHAR(16) NULL DEFAULT 'json'
fetched_at         DATETIME NOT NULL
deleted            BOOLEAN NOT NULL DEFAULT FALSE
```
Confirmed live (`GET /api/v1/Lookup/ByHash/{algo}/{hash}` on hasheous.org): a hit returns
far more than hash->name -- `signature.game`/`signature.rom` (DAT-sourced release-level
fields: region/language/year, a `demo` flag mapping straight onto `release.is_demo`,
category), plus a `metadata[]` array of cross-reference IDs *into every other provider*
(IGDB, TheGamesDb, MobyGames, ScreenScraper, SteamGridDb, ...) with per-entry confidence
voting. `provider_cache.rs` should use a Hasheous hit's `metadata[]` to bulk-populate
`title_provider_links`/`release_provider_links` for those other providers directly,
instead of treating it as just a `content_id -> name` cache -- this is the real reason
Hasheous goes first in implementation order, not just "hash-first is easiest."

**Title-keyed** (`igdb_games`, `mobygames_games`, `emuready_games`, `steamgriddb_games`)
-- these providers' own "game" concept is closest to `title` (region-independent), linked
via `title_provider_links`. Standard shape:
```
provider_id        <provider's own ID type, e.g. INT for IGDB, BIGINT for MobyGames>  PK
raw_response_path  VARCHAR(1024) NULL  -- NULL under raw_response_retention='discard'
raw_format         VARCHAR(16) NULL DEFAULT 'json'
title              VARCHAR(512) NULL     -- pulled out of the response for indexed search
fetched_at         DATETIME NOT NULL
deleted            BOOLEAN NOT NULL DEFAULT FALSE  -- e.g. a retracted/known-wrong cached entry
INDEX (title)
```
`emuready_games` additionally needs a child table -- its actually-useful payload is
compatibility/performance reports, which are per device+emulator, not one blob per game:
```
emuready_reports  emuready_game_id BIGINT UNSIGNED NOT NULL FK -> emuready_games(provider_id),
                  device VARCHAR(128) NOT NULL, emulator VARCHAR(64) NOT NULL,
                  rating VARCHAR(32) NULL, raw_response_path VARCHAR(1024) NULL,
                  raw_format VARCHAR(16) NULL DEFAULT 'json', fetched_at DATETIME NOT NULL,
                  PRIMARY KEY (emuready_game_id, device, emulator)
```
`steamgriddb_games` is really just a name -> SteamGridDB-id cache to avoid re-searching;
the grid/hero/icon art it returns gets downloaded straight into `media`
(`media.source = 'steamgriddb'`), not re-stored again here.

**Release-keyed** (`thegamesdb_games`, `retroachievements_games`) -- TheGamesDB's catalog
is flatter, closer to SKU-level than IGDB/MobyGames' abstraction, so it links via
`release_provider_links` instead; same standard shape as the title-keyed tables above,
minus the junction target.

`retroachievements_games` is the same shape and tier, live-integrated -- **not** through
`playmatch_deredis` (RA isn't one of its vendored providers) and **not** a vendor-and-patch
job like the other 7 (those are Rust; RA's own reference client below is Node.js). Instead
it's a from-scratch Rust client, ported from (not copied from) a real working reference:
[x3kim/RAChecker](https://github.com/x3kim/RAChecker) (MIT; credit for the original
project goes to x3kim -- the user's own fork is where the genre classification below was
contributed). That reference gives real value beyond just "an endpoint list":
- `API_GetGame`/`API_GetGameExtended` -- title, console, genre, release date, full
  achievement metadata; this is what actually fills `retroachievements_games`'s cached
  response.
- `API_GetGameList`/`API_GetGameHashes` -- MD5 hashes per console/game. A second candidate
  hash source alongside DAT imports for populating `hashes`/`local_rom_rahash` (see `local_rom_rahash`
  above) -- worth deduping against DAT-derived hashes at import time, not assuming one
  source is authoritative over the other.
- Auth via `z`/`y` query params (username + API key) -- needs `ra_username`/`ra_api_key`
  entries added to `app_root_ex/assets/api.env` when implemented (not yet present; per the
  standing rule, credentials live only there).
- Rate-limiting: serialized calls, configurable min interval, exponential backoff on
  429/5xx/network errors, no published numeric limit ("be gentle and cache aggressively")
  -- port this behavior into `provider_quota`/`provider_cache.rs` rather than reinventing.
- **Genre** (the user's own contribution to that repo): RA's `Genre` field is fine-grained
  free text (441 distinct subgenre strings, e.g. "2D Platforming", "Sports - Golf") --
  RAChecker normalizes it to one of 19 fixed major genres via a versioned
  (`GENRE_MAP_VERSION`) lookup table + keyword fallback. Port this mapping/logic into Rust
  as the normalizer feeding `genre`/`title_genres` for RA-sourced genre data specifically;
  the raw subgenre string is still preserved in `retroachievements_games`'s cached
  response (per `raw_response_retention`) even though only the normalized major genre
  lands in `title_genres`.

**Both** (`screenscraper_games`) -- ScreenScraper's own model is two-tier: a "jeu" (their
game entity, title-level, linked via `title_provider_links`) that internally lists "rom"
sub-entries, each carrying its own crc32/md5/sha1 -- and its real lookup API is hash-first
in practice (query by a dump's hash, get back which jeu and which of its rom sub-entries
matched). So `screenscraper_games` caches the jeu itself (standard title-keyed shape
above), plus a small hash-keyed index beside it so a `content` row resolves straight to
the right jeu without re-querying:
```
screenscraper_rom_index  content_id BIGINT UNSIGNED PK FK -> content(id),
                         jeu_id INT NOT NULL FK -> screenscraper_games(provider_id),
                         fetched_at DATETIME NOT NULL
```

Exact per-provider tier assignment above is a strong default, not a hard requirement --
worth revisiting per provider once actually integrated (`provider_cache.rs`), same as the
provider implementation order elsewhere in this doc.
### `title_provider_links` / `release_provider_links` (junctions, not per-provider FK columns)
Split in two because providers don't all model "a game" at the same tier -- IGDB's "Game"
is closer to `title` (region-independent); ScreenScraper/MobyGames/TheGamesDB key closer
to `release` (region/revision-specific). Exact per-provider tier assignment is an
implementation-time decision (`provider_cache.rs`), not fixed here. Same shape either way:
```
title_id / release_id   BIGINT UNSIGNED NOT NULL FK -> title(id) / release(id)
provider                VARCHAR(32) NOT NULL   -- 'igdb' | 'screenscraper' | ... matches Source::name()-style tags
provider_id              VARCHAR(64) NOT NULL   -- stored as text; each provider table's own PK type varies
confidence                ENUM('exact','hash','name','fuzzy','manual') NOT NULL
                          -- exact: the provider's own hash list matched (RA md5); hash: a hash
                          -- service's cross-reference (Hasheous/PlayMatch); name: a name search
                          -- whose normalized name equals ours (platform-checked for IGDB);
                          -- fuzzy: a name search's best guess; manual: the user
linked_at                  DATETIME NOT NULL
deleted                    BOOLEAN NOT NULL DEFAULT FALSE  -- a rejected/superseded match, kept rather than forgotten
PRIMARY KEY (title_id / release_id, provider)
```
A junction table instead of one nullable FK column per provider -- adding an 8th provider
later is a new row shape, not an `ALTER TABLE`.

**Handles "N releases share one provider identity" for free, no extra tier needed.**
RetroAchievements sometimes gives separate platforms their own achievement set but merges
different region/revision releases *on the same platform* into one set -- inconsistently,
data-driven, not a fixed rule. `PRIMARY KEY (release_id, provider)` only stops one release
from having two rows for the same provider; nothing stops two different `release_id`s from
pointing at the same `provider_id`. So that grouping is just N rows here with different
`release_id` and identical `(provider, provider_id)` -- `WHERE provider = ? AND
provider_id = ?` gets the group back directly, no dedup pass, no `release_major`-style
intermediate table. (RA is now live-integrated -- see `retroachievements_games` under
Per-provider cache tables -- but Hasheous's `metadata[]` cross-reference array also
includes RA, so a Hasheous hit can still cheaply pre-populate these links before/without
a direct RA call, e.g. to discover the RA game id before deciding a full RA fetch is
worth the rate-limit budget.)

### Media storage and cache (`media_store`, `media_cache`, `migrations/0006`)
**Roots.** A new file is written under the first configured `media_dirs` entry of its role
(`image`; `video` for videos; `audio` for BGM), else the app's `assets/media` (role `app`).
`media.root_role` + a `/`-separated `path` relative to that root; reads try every
configured root of the role. Rows from before the layout have `root_role` NULL and an
absolute path; the `media_relayout` job moves those (only files under the app's own media
dir -- anything else was not written by the app and is left alone), then moves every
laid-out file that is not where the layout puts it now: into a configured dir that was added
after it was written, and into its source folder. RetroAchievements tab images (icons, title
screens, badges, the avatar) go to `<image root>/_retroachievements/<RA's own path>` (the
provider cache without an image dir; files already there move over when next used).

**Layout.**
```
<root>/<Platform>/<Title>/<Region>/<source>/box_front.jpg              per release group
<root>/<Platform>/<Title>/<Region>/_prerelease/<source>/...           a demo/proto/beta's own art
<root>/<Platform>/<Title>/_title/<source>/cover.jpg                    region-less art
<root>/<Platform>/<Title>/_title/<source>/screenshots/<name>.jpg       screenshots (videos/, fanart/)
<audio root>/<original file name>.flac                                 BGM ("name (2).flac" on collision)
```
A release group = same title, platform, region, `norm_hash` and retail/pre-release kind,
i.e. a release and its revisions; they all link the one file. A retail group's art is also
linked to the same region's demo/prototype/beta releases for any slot they have nothing in.
Title-level provider art (an IGDB cover) goes to `_title` and is linked to the title's
releases as before; the grid shows the best-ranked release's art (`ReleaseRank`), a manual
pick first. Path components are sanitized for every OS (`<>:"/\|?*`, control characters,
trailing dots/spaces, CON/NUL/COM1..., 100 characters). `<source>` is the provider
(`igdb`, `steamgriddb`, `retroachievements`, ...) or `user` for uploads, so each source's
art sits side by side under the slot's fixed name and `media_priority` can be changed
without moving files. A provider download replaces a file only unlinked rows point at, and
otherwise takes `_2`, `_3`; it never replaces a file no row knows. An upload whose name is
taken is refused with a conflict naming the file, and the user chooses to overwrite it,
have it renamed (`on_conflict`), or cancel. Identical bytes (`media.content_sha1`) are
stored once and linked again.

**Manual media.** `source = 'manual'` rows win: shown first, and `attach_media` never adds
to or replaces a slot holding one. Removing unlinks (and retires an unlinked row); nothing
deletes a file.

**Cache.** `media_cache(media_id, variant, tier, rel_path, bytes, last_access)`; variants
`full`, `thumb:avif`, `thumb:jpeg`. Full size is served tier 1 -> tier 2 -> original,
copied into tier 1 on a miss. A thumbnail (longest edge `media_thumb_px`, default 320) is
generated on first request from the best copy and kept in tier 1, else tier 2, else the
app's `assets/media/_cache` (tier 0, never evicted). Each insert evicts the tier down to
its cap (`media_cache_tier{1,2}_max_mb`, 0 = none) least recently used first, every
full-size copy before any thumbnail; only cache copies are ever removed.

**Thumbnail formats.** `image` crate (jpeg/png/gif/webp decode, `avif` encode via ravif --
pure Rust, builds for every target -- and `rayon`): lossy AVIF (default,
`media_thumb_quality` 60) or JPEG. WebP and JPEG XL are placeholder arms of `ThumbFormat`
that fall back to JPEG until an encoder is added. `?format=` overrides per request (e.g. a
mobile client that finds AVIF decoding heavy). AVIF *sources* decode only with the optional
`avif-decode` feature (dav1d, C); without it such a source is served as-is.

### `jobs` (background worker queue)
```
id            BIGINT UNSIGNED PK AUTO_INCREMENT
job_type      ENUM('dat_sync','media_download','provider_query','provider_bulk_sync','file_scan','hash_match','db_optimize') NOT NULL
status        ENUM('pending','in_progress','done','failed') NOT NULL DEFAULT 'pending'
payload       JSON NOT NULL          -- job-type-specific args (game_id, provider, path, ...)
attempts      SMALLINT UNSIGNED NOT NULL DEFAULT 0
priority      SMALLINT UNSIGNED NOT NULL DEFAULT 0   -- 0 background; higher first, in the lane's :fast slot
last_error    TEXT NULL
claimed_at    DATETIME NULL
updated_at    DATETIME NOT NULL
INDEX (job_type, status), INDEX (status, priority, id)
```
Priority: the scheduler orders pending jobs by `priority DESC, id`, and a priority job runs
in its lane's `:fast` slot next to the lane's background job (the in-process rate limiter
still serializes the provider's requests). Used for the metadata/art refresh of a page the
user just opened and for manual links, so they never queue behind a bulk sync.
Worker claims with `UPDATE jobs SET status='in_progress', claimed_at=NOW() WHERE id IN
(SELECT id FROM jobs WHERE job_type=? AND status='pending' ORDER BY id LIMIT ? FOR UPDATE
SKIP LOCKED)` -- standard pattern, no extra queue library needed.

`db_optimize` runs the deletion-policy hard-delete pass described at the top of this
section (bottom-up: `content`/`local_files`/`local_rom`/`local_game` checked against on-disk presence,
`release`/`title` checked against live references from above) -- scheduled periodically
like `dat_sync`, not triggered on every soft-delete.

`provider_bulk_sync` runs a provider's `full`/`by_platform` sync (see Provider sync
strategy, above) -- payload carries the provider and, for `by_platform`, the platform(s).
Complements rather than replaces `provider_query` (per-item, on-demand enrichment still
runs on top, same as RAChecker's own bulk-then-enrich pattern).

### `provider_quota`
Replaces what ScreenScraper/TheGamesDB tracked in Redis before the `playmatch_deredis`
de-redis pass -- the deferred piece from that work, now with a real home.
```
provider        VARCHAR(32) PK
remaining       INT NULL
resets_at       DATETIME NULL
updated_at      DATETIME NOT NULL
```

### `sources` (dat_download.rs's six sources)
```
id    TINYINT UNSIGNED PK AUTO_INCREMENT
name  VARCHAR(32) UNIQUE NOT NULL   -- matches dat_download::Source::name()
```
DAT-derived hash/name data continues to land in `content`/`hashes` tagged by which
`source_id` last confirmed it, same shape as the old schema's `games.source_id` had, just
rehomed onto `content`.

## Module layout (`src/database/`)

```
mod.rs               -- re-exports, matches existing doc-comment style
config.rs             -- DatabaseConfig (recreate, same shape as before: host/port/user/...)
connection.rs          -- connect()/migrate() (recreate)
error.rs               -- DatabaseError (recreate)
dat_download.rs         -- existing, unchanged
dat_parse.rs            -- recreate: DAT XML -> ParsedDatFile, unchanged design from before
import.rs               -- recreate, targets the new content/hashes/files shape instead of old games/files
lookup.rs               -- recreate: hash -> content/game lookup
provider_cache.rs        -- new: check-then-fetch against playmatch_deredis clients, writes *_games tables;
                            query order at runtime reads the configurable priority setting, not a
                            hardcoded order
jobs.rs                  -- new: enqueue/claim helpers for the jobs table
```
```
../../migrations/0002_content_rom_game.sql   -- the schema above, additive to 0001 or replacing it
                                              -- (0001's tables are superseded; recommend dropping
                                              -- and replacing rather than keeping both live)
```
A new binary crate/target `romexplorer-worker` (exact location TBD at implementation time
-- either `src/bin/romexplorer_worker.rs` or a small sibling crate) holds the worker's
main loop: poll `jobs`, dispatch by `job_type`, dat-sync on a timer, media-download queue
paced against `provider_quota`.

## Deferred (not this pass)
- Exact per-provider column sets beyond `raw_response_path`/`raw_format`/`title`/
  `fetched_at` -- add as concrete query needs emerge, not upfront.
- Exact storage shape/location for the `raw_response_retention`, provider-priority, and
  sync-strategy settings -- all flagged as sharing one undecided settings mechanism.
- MobyGames credential (`mob_api_key` in `api.env` is still a placeholder).
- The worker's exact launch mechanism (spawned by the main app vs. started separately by
  the user) -- functionally works either way given it only talks to MariaDB.

## Verification
`tests/src/test_database.rs` (`cargo test --test test_database -- --nocapture`) runs the
whole chain in plan order against the bundled `mariadbd` on a throwaway data dir: server
bootstrap/start -> `migrations/0001_schema.sql` + platform seeding -> jobs round trip ->
DAT download (bandwidth-limited: a few files per GitHub/redump source, the 100-150MB
no-intro/tosec packs only with `ROMEXPLORER_DB_TEST_BIG_DOWNLOADS=1`) -> parse + import of
a per-source subset (`ROMEXPLORER_DB_TEST_FULL_IMPORT=1` for everything) incl. an
idempotent re-import check -> the four lookup scenarios (existing hash; made-up hash +
real title; real hash + random title; neither) built from the imported data at run time
-> one single + one list request per provider whose credentials exist -> one media file
per provider, spaced out -> `by_platform` bulk sync last (IGDB one page, RA one console,
on a small platform) -> `SHUTDOWN`. Report: `tests/results/database_end_to_end.md`
(git-ignored -- it carries real names/hashes). Unit tests cover the parsers, name
parsing/normalization/scoring, platform mapping, credential parsing, the raw store and
the RA genre folding (`cargo test --lib database`).

## Implementation status

Landed in `src/database/` (module doc comments carry the detail):

| plan item | where | notes |
|---|---|---|
| embedded server | `server.rs` | Rust bootstrap (`mariadbd --bootstrap` fed the bundled `share/*.sql`, same option set as `mariadb-install-db`, which is shell-only and needs `my_print_defaults` we don't ship), start, `SELECT 1` readiness, SQL `SHUTDOWN`. Needed `share/` (bootstrap SQL, `errmsg.sys`, charsets) and the vendored `libssl/libcrypto` copied into `app_root_ex/lib/<os>/mariadb/` -- the build scripts don't install those yet (see README). |
| schema | `migrations/0001_schema.sql` | replaces the old 4-table `0001_hash_lookup.sql` outright (nothing was deployed). |
| config / paths / creds | `config.rs`, `env_creds.rs` | `AppPaths` = the `app_root_ex/` layout; `api.env` is the only credential source, values never logged/echoed. |
| DAT download | `dat_download.rs` | all six sources, ported from `scripts/fetch_*.sh` (no-intro's 3-step POST flow included). |
| DAT parse / import | `dat_parse.rs`, `import.rs`, `platform_map.rs` | batched multi-row upserts; multi-disc DAT lines roll up into one `release`; structured `<release region>` preferred, `matching::parse_name` backfills. |
| matching | `matching/` | `name_parse`/`util`/`scoring` ported from playmatch (MIT) + extensions; `resolve` = hash-first, name-second. |
| providers | `providers/` | 7 vendored clients + own RA client (`retroachievements.rs`, ported from RAChecker's shape), `cache.rs` (per-provider tables + links), `raw_store.rs`, `bulk.rs` (IGDB by_platform paging via an added `get_games_by_platform`, RA by console with hashes), `media.rs`, `ra_genre.rs`. |
| worker | `src/bin/romexplorer_worker.rs` | claims per `job_type`, dispatches `dat_sync`/`provider_query`/`provider_bulk_sync`/`media_download`/`db_optimize`; `file_scan` is a logged stub (lands with the GUI/scanner). |
| settings | `settings.rs` | the undecided mechanism is now a `settings(key, JSON)` table: `provider_priority`, `provider_sync_scope`, `raw_response_retention`, `cache_ttl_days`. |
| dev CLI | `src/bin/romexplorer_dbtool.rs` | `serve` (embedded server until Ctrl-C), `shutdown`, `init`, `import <source> <path> [--member ...]`, `virtual-collection`, `counts`. |
| credential/user setup | `server.rs`, `config.rs`, `connection.rs` | `MariaDbServer::set_root_password`/`create_user` (`UserPrivileges::Admin` = `ALL PRIVILEGES ON *.*` since `CREATE USER` is global-only, no database-scoped equivalent; `App` = `SELECT/INSERT/UPDATE/DELETE` on just the target database); `DatabaseConfig::with_user`/`with_password`/`with_host` builders; `connection::initial_setup` bootstraps+starts the server, runs `initialize()` as the initial passwordless root, then locks root down and creates an admin (+ optional separate app) account -- never logs/returns a password. Covered end to end by `tests/src/test_database.rs::initial_setup_end_to_end` (local-only, own throwaway server). |
| directories / media / temp dirs | `migrations/0001_schema.sql` | schema only this pass (`local_directories`, `files.directory_id`, `media_dirs`, `temp_dirs`) -- CRUD modules and the scan-to-DB write path are follow-up work, see Deferred below. |
| source-tagged metadata | `migrations/0001_schema.sql` | schema only this pass (genre hierarchy + `genre_raw_mapping`, `source` on the three title junctions, `release_provider_serials`, `title_locales`, `release_ratings`, player counts) -- population/resolution logic (the hash-match job, `metadata_source_priority`/`title_display_mode` settings consumers) is follow-up work. |
| `local_*` rename + `api` layer | `migrations/0001_schema.sql`, `database/directories.rs`, `database/storage.rs`, `src/api/`, `src/server/`, `romexplorer_server` | Ownership tables are now `local_directories`/`local_files`/`local_rom`/`local_rom_files`/`local_rom_rahash`/`local_game`/`local_game_roms`/`local_release_games` (docs/plan/api.md's `local_`/`remote_` convention; catalog tables unprefixed). `0001` was edited in place (pre-release), so a data dir created before the rename needs recreating. The REST server + browser console over the new `api` layer is described in docs/plan/api.md's own Implementation status. |
| virtual collection | `virtual_collection.rs` | synthesizes an owned collection (`local_files`/`local_rom`/`local_game` + junctions, zipped singles with `content_contents`, multi-disc sets with m3u, duplicate copies missing a disc) from catalog rows under a fake `/virtual/romexplorer` root, for GUI/REST sorting-filtering work before the scanner exists. The app DB (`app_root_ex/data/mariadb`, port 3307) currently holds one: 302 games over 10 platforms (PSX/PS2/GC/DC/Saturn/Wii disc sets as cue+bin, iso, chd, rvz; GBA/NES/SNES/Genesis cartridges, ~1/3 zipped), 57 multi-disc, 2 duplicates each missing a disc -- `tests/results/virtual_collection.md` has the breakdown. Regenerate with `romexplorer_dbtool virtual-collection --replace`. |

Schema deltas vs. the sections above (all additive, driven by implementation needs):
- `release_contents (release_id, content_id, file_name)` -- the catalog-side junction the
  hash lookup path needs (`hashes -> content -> release_contents -> release -> title`);
  the plan only had the ownership-side `local_rom_files`/`local_game_roms`.
- `release` gained `name_hash`/`norm_name`/`norm_hash` (full-length unique/match keys --
  InnoDB can't index 512 utf8mb4 chars), `languages`, `year`, `serial`, `category`,
  `disc_count`, `is_unlicensed`; `title` gained `norm_name`/`norm_hash`; `local_files` gained
  `path_hash`; `content` gained `source_id`. `app_platform_id` is `NOT NULL DEFAULT 0`
  (`Platform::Unknown`), not nullable.
- `dat_imports` -- which DAT files/pack members were imported, with counts.
- `retroachievements_hashes (md5 -> RA game/console)` -- RA's bulk hash list, the thing
  that links RA sets to catalog releases by md5 without per-game calls.
- `hasheous_lookups.hit`, `retroachievements_games.{console_id,num_achievements,genre_raw}`,
  `media.source_url` + an `icon` media type, `emuready_games.provider_id` as VARCHAR (their
  ids are opaque strings), `game.cover_media_id` (was `cover_file_id`).
- `media` gained `root_role`, `content_sha1`, `bytes` (`0006`); `media_cache` (`0006`);
  `provider_sync_state` (`0004`) and `provider_dump_files`, `schedules`, `igdb_games.updated_at`,
  `jobs.cancel_requested` + status `cancelled` (`0005`); job types `playmatch_fill` (`0005`)
  and `media_relayout` (`0006`); `thegamesdb_games.{remote_platform_id, platform_name, app_platform_id}`
  (`0007`, filled by the dump import / by-platform sync; an unchanged dump diff backfills
  them once from the local dump file).
- `ra_user_profile`, `ra_user_progress`, `ra_user_game_detail`, `ra_user_achievements`,
  `retroachievements_games.{console_name, image_icon, image_title, image_ingame, image_boxart}`
  and job type `ra_account_sync` (`0008`): the RetroAchievements account's progress for the
  RetroAchievements tab.
- One release per platform + name (`0011` + `release_merge`): releases used to be keyed by
  DAT source too, so a disc listed by redump (cue/bin), mame-redump (CHD) and the RA hash list
  was three releases. `release_sources` records every DAT listing a release;
  `release_contents.source_id` keeps each source's file list apart (a release's dumps), so a
  local copy matching any dump -- cue/bin, CHD, or a CHD/RVZ's hashed inner data -- is that
  release. The one-time merge (in Rust after the migrations, resumable) folded 114k duplicate
  rows, re-pointing titles, links (stronger kept), media, local games, sets, serials and
  ratings; `uq_release_identity` is now `(app_platform_id, name_hash)`.
- `retroachievements_games.{genre_major, genre_checked_at}` and job type `ra_genre_fetch`
  (`0009`): RA genres fetched game by game for the tab's genre filter.
- `title_aliases`, `title_merge_log`, `title_merge_suggestions`, job types `title_dedupe` /
  `igdb_reapply` (`0012`): title merging (`database::title_merge`).
- `title_alt_names` and an index on `title_locales.name` (`0013`); `0013` also backfills
  RetroAchievements exact links from RA-listed `ra_hash` values.
- `title_merge_archive`, `title_merge_blocks`, `title_merge_log.undone_at` / `restored_title_id`
  (`0014`): merged titles are soft-deleted and archived as they were (row + their rows in every
  title-keyed table, JSON), so a merge can be decoupled exactly; decoupled pairs never
  auto-merge again.
- `release.is_budget` / `is_special_edition` / `is_aftermarket` / `revision_sort`,
  `title_releases.sort_weight` / `sort_set_at`, `data_migrations` (`0015`): edition flags and a
  numeric version key from name tags (backfilled once by `release_editions::backfill`), and the
  user's manual release order per title (float weights on the link row, renumbered when gaps
  collapse -- `release_order`); the weight moves with the release on merge / decouple.
- `ra_game_hashes`, `ra_patches`, `ra_patch_files`, dir type `patch`, job type `ra_patch_download`
  (`0016`): RetroAchievements' per-hash list (name, labels, compatibility patch URL) and the
  patch archives downloaded from it, hashed with their members (`database::ra_patches`).
- `content_contents.member_path` (`0017`): a member's path inside its archive (or a track's name
  for a sheet); scan / rehash record archive members and sheet tracks.
- Titles are keyed by normalized base name *within a platform* at import; cross-platform
  merging (simultaneous releases) vs. keeping ports apart is left to a later
  provider-informed step, since a DAT can't tell the two cases apart.
- `local_directories` (new, self-referencing: collection roots and the subdirectories inside
  them that need their own `dir_type`/`app_platform_id`/`is_archival` setting) + `local_files`
  gained `directory_id` (nullable FK, `NULL` for rows with no configured directory, e.g.
  `virtual_collection.rs`'s synthetic ones). Real absolute paths throughout, not
  root+relative -- a relative (sync-friendly) path is computed on demand by stripping the
  owning `directories.path` prefix, not stored separately.
  Hierarchy rules, enforced by `api::directories` (the only writer):
  - Any number of collection roots (e.g. redump and no-intro sets kept apart, or one per
    drive), never one inside another.
  - Every other row lies inside a root, so none can be added before a root exists.
  - `parent_directory_id` is derived, never chosen: the nearest configured row containing
    the path. It is NULL exactly for roots, and `database::directories::relink_parents`
    keeps it current as rows come and go.
  - Removing a row with configured subdirectories is refused unless `recursive`, which
    soft-deletes them too. Configuration rows only -- nothing on disk is touched.
  - Paths are stored canonical (`canonical_path`: no trailing separator; on Windows `\`
    and an uppercase drive letter) and compared case-insensitively on Windows/macOS
    (`same_path`/`path_contains`), component-boundary aware.
- `media_dirs` (role: `tier1_cache`/`tier2_cache`, at most one active row each,
  enforced at the Rust layer, plus multi-row `image`/`video`/`audio`) and `temp_dirs`
  (tier 1/2) -- app-infrastructure storage config, independent of any collection root, not
  rows in `local_directories`. `AppPaths::mariadb_data_dir`/`provider_cache_dir` gained
  `ROMEXPLORER_DATA_DIR`/`ROMEXPLORER_CACHE_DIR` env overrides.
- `genre` gained `parent_genre_id` (self-referencing hierarchy, no fixed tier count) +
  new `genre_raw_mapping(source, raw_text -> genre_id)` generalizing `ra_genre.rs`'s
  hardcoded keyword rules into data (RA becomes its first populated source, not a special
  case -- the mapping itself still needs curating per source, not done this pass).
  `title_genres`/`title_franchises`/`title_companies` all gained a `source` column in
  their PK -- the same title's genre/franchise/company from two sources are kept as two
  rows even when they resolve to the same target, never merged or overwritten; which
  source's value to *display* on overlap is a `settings` key
  (`metadata_source_priority`), resolved at query time.
- `release_provider_serials(release_id, provider -> serial)` -- a provider's own serial,
  kept separately from `release.serial` (stays DAT-sourced, unchanged). `release` gained
  `min_players`/`max_players`. New `release_ratings(release_id, board -> rating)` junction
  for per-region age-rating boards (ESRB/PEGI/CERO/USK/...), multi-value by nature.
- `title_locales(title_id, language, region, source -> name)` -- localized title names;
  generic enough that a deferred Japanese-name-for-US/EU-release pass is just inserted
  rows later, no further migration. Display mode (`title_display_mode`: all-English /
  all-local / a custom language-priority list) is a `settings` key, resolved the same way
  as the metadata-source-priority settings above.
- `jobs.job_type` gained `hash_match` (payload/dispatch logic not yet implemented --
  needs the not-yet-built scan orchestrator's write path first, see `docs/plan/
  rom_module.md`).
- `collection_sets` / `collection_set_releases (set_id, release_id, position)` -- user-made
  ordered lists of releases (docs/plan/api.md, "collection sets").
- `release.region`/`languages` are always the codes `Region::code`/`Language::code`
  produce, whether they came from name parsing or a structured DAT attribute
  (`import::normalize_structured_tags`); a language word in a `region=` attribute (RA's
  DATs) lands in `languages`.

Deferred from this pass:
- File scanning writes the ownership tables (`local_files`/`local_rom`/`local_game`) through
  `api::scan` (whole-file hashes, one rom per candidate, DAT hash-match) -- see
  docs/plan/api.md's Implementation status for what that first pass does and doesn't do.
- ScreenScraper needs developer credentials (`screenscraper_dev_id`/
  `screenscraper_dev_password`) which aren't in `api.env` yet; RetroAchievements needs
  `ra_username`/`ra_api_key` (also not yet present); MobyGames' key is still a placeholder.
  Their clients build only when those keys appear -- nothing else changes.
- TheGamesDB/ScreenScraper media: the vendored clients don't expose their image endpoints.
- The full RAChecker subgenre lookup table (`ra_genre.rs` carries the keyword-rule half).
- `db_optimize` is the bottom-up purge from the deletion policy, not yet scheduled
  automatically; `dat_sync` staleness scheduling (the TTL) is a setting but no timer
  enqueues it yet.
- `local_directories`/`media_dirs`/`temp_dirs` CRUD modules (insert/resolve-ancestor-chain/list,
  the "at most one active tier1/tier2 row" Rust-level rule) -- schema landed, no Rust API
  yet. `FileScan`'s `full`/`new_only`/`rehash` write path -- needs the not-yet-built scan
  orchestrator Layer 3 (`docs/plan/rom_module.md`) to produce the candidate list it would
  consume. `hash_match`'s dispatch logic (DAT/Hasheous/cross-ref-following/file->rom->
  game construction, reusing `virtual_collection.rs`'s disc-grouping shape adapted to real
  scanned files) -- individually substantial, designed but not implemented. A
  find-missing-rom job -- not yet named/designed beyond "exists later". `raw_store.rs` staging through
  `temp_dirs` before its permanent `provider_cache_dir()` home, and `SingleArgs.temp_dir`
  becoming tier-aware -- both still single-path, unaffected until wired up.
