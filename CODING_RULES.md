# Coding rules

Project-wide conventions that govern how code/scripts/schema get written here. This is the
single place they live -- not README.md (project/submodule documentation), not code
comments (which explain what a specific piece of code does, for a human reading it, and
should stay that way rather than restating policy). When a rule and a piece of code could
both plausibly explain "why", the rule goes here and the code comment at most points here.
Do not hardcode paths including usernames. Use relative paths for project or app paths.

## Version control

The user runs all git commands themselves. Never run a git write command (`add`, `commit`,
`checkout`, `mv`, `rm`, `submodule add`, etc. -- including a "harmless" revert like
`git checkout -- <file>`) or any other action that changes repo/commit state. Plain
filesystem commands (`mv`, `cp`, `rm`, `patch -R`) are fine for working-tree changes; when
a git action is actually needed, give the user the exact command to run.

## Building and testing on the user's Windows PC

Build and test only inside the repo folder itself. Norton quarantines builds made anywhere
else on that PC, including a sibling worktree or `%TEMP%`. A scratch checkout, a separate
target dir or test output goes in a subfolder of the repo whose name starts with `.test`
(`.test`, `.test-music`, ...), which `.gitignore` keeps out of git. Any other folder name
must start with `.` and be added to `.gitignore` in the same change.

## Parallelism

Default thread/worker counts always come from `available_parallelism()` (or the
project's own `native_lib::recommended_reader_threads`/`physical_core_count`, which
already wrap it correctly for asymmetric P/E cores) -- a caller may override, but nothing
in this codebase hardcodes a thread cap.

## Provider credentials

Every provider credential (API keys, dev/user account pairs) is read from
`app_root_ex/assets/api.env` and nowhere else -- never hardcoded, never written to a
config file this project generates, never logged, printed, or included in an error message.
`database::env_creds::ApiCredentials` is the only reader; its `Debug` impl shows key names,
never values. A value may be sealed (`enc:v1:...`, `database::vault`) and is opened on load;
the only thing that ever writes `api.env` is the explicit `romexplorer_server
encrypt-credentials` command, which seals it in place.

Any other credential kept in the database -- a peer's token, a password hash -- is listed in
`api::secrets::SECRET_COLUMNS`, so no generic reader (the DB viewer, a backup, the mobile
cache) ever returns it. A unit test fails on a credential-named column that isn't
classified. A provider whose credentials are absent/placeholder is simply unavailable
(its client doesn't get built) -- that's a normal, silently-handled state, not an error to
surface loudly.

## Submodule vendoring

Submodule code stays as close to its own upstream as possible:
- Each submodule is the project's own upstream repository, under `external/<name>/`, pinned
  to a commit. All of this project's own material for it lives in `deps/<name>/`, never in
  the checkout: build scripts in `deps/<name>/scripts/`, build output in
  `deps/<name>/release/<platform>/<arch>/<version>/{bin,shared,static,include}/`,
  scratch/build state under `deps/<name>/_build/`.
- Prefer a `deps/<name>/patches/*.patch` (applied at build time, idempotently, `patch -p1`,
  dry-run first, loud failure on conflict; mark the submodule `ignore = dirty` in
  `.gitmodules`) or a new file under `deps/<name>/` spliced in non-invasively (CMake
  `CMAKE_PROJECT_INCLUDE`, a separate `Makefile.*`, our own `CMakeLists.txt`) over editing
  the submodule's tracked files. Never commit to the submodule itself: a change that has to
  be there is a patch.
- Each submodule's own license is copied to `app_root_ex/lib/LICENSES/<name>.LICENSE` for
  distribution alongside the binaries built from it.
- A Rust-to-Rust vendor (e.g. `playmatch_deredis`) follows the same patch convention as the
  C/C++ submodules: copy the upstream file, diff it, keep the diff as a patch under
  `patches/`, with a script (`scripts/sync_from_upstream.sh`) that re-applies patches on top
  of a fresh upstream pull and fails loudly (not silently) if a patch no longer applies.
  A vendor target in a different language (e.g. this project's RetroAchievements client,
  ported from a Node.js reference) is a port, not a vendor-and-patch job -- say so in the
  module doc comment and credit the reference's license under `app_root_ex/lib/LICENSES/`
  even though no patch file results.

## Native binaries and app layout

Bundled native binaries/libraries live per OS under `app_root_ex/lib/<os>/`
(`linux`/`windows`/`macos`/`ios`/`android` -- `database::config::AppPaths::os_dir_name()`
picks the one the current build targets), mirroring each submodule's own
`deps/<name>/release/<platform>/` layout. The whole `lib/*/mariadb` tree (and any other per-OS
build output under `lib/<os>/`) is build output, not source -- git-ignored, reproduced by
the submodule's build script.

`app_root_ex/lib/<os>/mariadb/` specifically needs, beyond `bin/`/`shared/` from
`build_mariadb_server.sh`: `shared/libssl.so*`+`shared/libcrypto.so*` from the vendored
OpenSSL build (`mariadbd` won't start without them, linked via its `$ORIGIN/../shared`
rpath), and `share/` from the MariaDB install staging tree (the bootstrap SQL files,
`share/english/errmsg.sys`, `share/charsets/`) -- currently copied in by hand; both should
be added to `build_mariadb_server.sh`'s own assembly step.

Machine-local runtime state -- the embedded server's data dir (`app_root_ex/data/`),
downloaded DATs/cached provider responses/media (`app_root_ex/assets/{download,cache,
media}/`) -- is git-ignored; never assume any of it is present in a fresh checkout.

## Tests

Test binaries live under `/tests` (or the project root, matching existing `[[test]]`
entries in `Cargo.toml`): source in `tests/src/`, shared test helpers in
`tests/src/support/`, generated output as Markdown in `tests/results/` (git-ignored when
the report can contain real sample data -- see "Sample data" above). A sample-backed test
skips cleanly (doesn't fail) when its samples root isn't populated, rather than requiring
every dev to have the full sample set.


## Job progress: always detailed, from the first second

A running job must never sit at 0 with nothing said. Every job's progress (`jobs::Progress`)
says, at all times:

- **what it is doing right now** -- the step and the item ("merge 312 of 4,180", "reading
  game 2,001 of 9,500", "removing deleted files"), not just the job's name;
- **how many in total** and **how many done**, in the unit the step counts (titles, merges,
  links, files, bytes), with running results where there are any ("(12 split)");
- for a job made of steps, **which step of how many** -- use `jobs::Steps`, which names each
  step, gives it a slice of the bar and lists how each one came out. Add each step key's
  label to `step_label` in **both** UIs (`romexplorer-gui/src/screens/jobs.rs`,
  `stepLabel` in `console.html`) and to every `app_root_ex/assets/i18n/*.json`.

In practice:

- `force` a message *before* every query, download or fetch that can take more than a
  moment, saying what it is ("reading the automatic merges"); a phase that is one big
  statement still says which statement of how many ("removing deleted files (3 of 9)").
- In a loop, `report(i, Some(n), ...)` on every item -- it is throttled to one write a
  second already, so no `i % 200` gates that leave the bar still for minutes on slow items.
- `None` as the total only while the total is genuinely unknown (walking a folder), and
  then the message carries the running count ("12,400 file(s) found so far").
- The worker writes "<job>: starting" when it claims a job, so the row is never blank; a
  job's own first report must follow before any long work, not after it.
- A new job type is not done until its progress has been watched once on real-sized data:
  if the row stays on one message for more than a few seconds, that phase needs reports.

## Database schema (`migrations/*.sql`)

- Soft-delete (`deleted` column) on every entity table; hard deletes only happen in the
  `db_optimize` background-worker job (`database::jobs`), never inline with normal writes.
  Pure junction tables have no `deleted` column -- a relationship either exists or is
  replaced, there's nothing to soft-delete.
- `app_platform_id` columns are always `common::models::Platform`'s own id (`platforms.id`,
  seeded from the enum by `database::connection::seed_platforms`) -- never a provider's id
  (e.g. IGDB's), which lives in a separate, explicitly-named cross-reference column instead.
- A `*_hash` `CHAR(40)` column is the sha1 hex of a text key, used only to give a long
  `VARCHAR` column (too long for InnoDB to index directly in utf8mb4) a real unique/lookup
  key.
