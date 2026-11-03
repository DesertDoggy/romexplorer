# App GUI: Dioxus (desktop, web, mobile)

**Status: implemented at console parity** (2026-09-28). Every console screen has a Dioxus
counterpart in `romexplorer-gui/`: the web build is served by `romexplorer_server` at
`/app/`, the desktop build starts or finds a server on this machine. Mobile builds are not
made yet; the layout, touch input, connect screen and offline cache seam are in place for
them. Companion to `docs/plan/api.md` (the REST API this GUI speaks).

## Context

The web console (`src/server/console.html`, ~2,400 lines of JS, ~30 screens) became the
real UI while the backend grew. The GUI was asked to have the **same layout structure**,
run on Windows / macOS / Linux and later iOS / Android, lay out adaptively, and stay
separable into a completely different phone UI should adaptive layout not be enough.

## Decision: Dioxus

Chosen over Tauri, Electron and the native-Rust toolkits (egui/iced/slint/gpui):

- **100% Rust**, one crate for desktop, web (WASM) and mobile.
- **Webview for free.** Dioxus renders through `wry` (the layer Tauri uses): a real browser
  engine, so `<video>`/`<audio>`, embedded guide websites and a PDF reader (`pdf.js`) need
  no hand-rolled rendering -- and the console's HTML/CSS carries over as is.
- **Traded away:** Tauri's larger plugin ecosystem (tray, auto-update, notifications are
  less polished in Dioxus). Acceptable.

Known risk still to test: **WebKitGTK on Linux** (codec coverage for BGM/video depends on
the distro's GStreamer plugins). Fallback if it fails: native playback (`gstreamer-rs` or an
`mpv` child window) while the webview does everything else.

## How the GUI reaches data: the REST API, everywhere

```
romexplorer-gui (desktop / web / mobile)
   └── REST + bearer token ──> romexplorer_server ──> api::* ──> MariaDB
```

The first plan had desktop call `api::*` in-process. It was dropped for one contract:

- **Parity is exact.** The GUI reads the same JSON the console does; screens were ported
  function by function from the console's JS, field names unchanged.
- **Mobile can't host the backend.** The embedded MariaDB does not run on a phone:
  on Android it would need Termux (an app can't depend on it; since Android 10 an app
  can't exec binaries it downloaded, and the OS kills background servers), and iOS can't
  run a separate server process at all. So phones must be REST clients anyway.
- **Desktop pays one loopback hop**, which the webview needed anyway for media URLs. It
  finds the server on `127.0.0.1:7337` or starts `romexplorer_server` (beside the exe, or
  the workspace `target/`), waits up to 90 s, and reads `data/server_token` (`boot.rs`).
  The server outlives the window, like the worker it hosts (stop it in Config › Server).
- A later in-process `LocalBackend` (the axum router called with `tower::ServiceExt::oneshot`,
  no socket) remains possible as an optimization: the screens only see `api::get/post/...`.

### Why not SQLite instead of MariaDB (asked for the phone case)

Not a small change: ~511 queries in 45 files, ~190 of them MariaDB-specific
(`ON DUPLICATE KEY UPDATE`, `INSERT IGNORE`, multi-table `UPDATE … JOIN`, `FIELD()`,
`REGEXP`, `GROUP_CONCAT`, `JSON_ARRAYAGG`, `LAST_INSERT_ID(id)` tricks), migrations using
`ENUM` / `ALTER … MODIFY`, and `MySqlPool` in ~290 places. Every piece has a SQLite
equivalent, but it's a port of the whole data layer plus a second migration set -- weeks,
not a night -- and SQLite's single writer would slow the parallel worker lanes on a
catalog of 1.2M releases / 3.3M contents / 9.9M hashes. A phone doesn't need the whole
catalog; it needs what the user browses (below). A read-only "offline pack" export (one
platform's titles/releases as a SQLite file) stays possible later.

### Offline cache (phones)

`cache.rs` is the seam: every successful GET is stored under its path; when the server
can't be reached, a GET answers from the cache. Today the store is in memory for the run
(`MemoryCache`, 400 entries). The phone store will be a persistent one behind the same
`ResponseCache` trait -- a key → JSON SQLite table (`rusqlite`, bundled) plus thumbnails,
with "pin" for lists to keep. No screen changes.

## Project layout

- Root `Cargo.toml` is a workspace: `members = [".", "romexplorer-gui"]`,
  `default-members = ["."]` (plain `cargo build/test` still builds only the backend).
- `romexplorer-gui/`: `Dioxus.toml` (`base_path = "app"`), features `desktop` (default),
  `web`, `mobile`.
  - `assets/app.css`: the console's `<style>`, **verbatim** (same classes, same look).
  - `assets/mobile.css`: the app's own additions (phone layout, touch targets, the
    filter toggle).
  - `src/api.rs`: endpoint + token, GET/POST/PUT/PATCH/DELETE, multipart upload, JSON
    accessors (`s`, `u`, `b`, `arr`, `items`), saved endpoint (web: localStorage; native:
    `<config dir>/romexplorer/gui.json`).
  - `src/cache.rs`: the offline cache seam.
  - `src/html.rs`: the console's display helpers (badges, flags, colors, glyphs) as
    HTML strings; `src/state.rs`: vocabulary + settings; `src/ui.rs`: toasts, menu,
    modal, popover, confirm/prompt.
  - `src/components/`: `Grid` (the console's `makeGrid`: list/card modes, sort, filters,
    pager, selection by checkbox / ctrl-click / drag rectangle, context menu on the
    selection, hover popover, row drag-reorder, resizable columns, per-page memory of
    search/filters/sort/page), `Combo` (type-to-filter pulldown with chips).
  - `src/screens/`: one module per area -- `titles`, `releases`, `collection` (games,
    roms, files, contents, sets), `ra`, `edit`, `jobs`, `config`, `config_copy`, `copy`,
    `settings`, `data`, `status`, plus `common` (catalog filters, set / keyword actions,
    release columns, Open-with) and `widgets` (media slots, manual match, RA card, merge
    tools).
  - `src/main.rs`: routes (the console's hash routes as paths: `/collection/titles/12`),
    the shell (header, tabs, subtabs, overlays), the connect screen.

## Adaptive layout, and splitting off a phone UI

One app; `mobile.css` switches below 700 px: the tabs become a bottom bar, subtabs and wide
tables scroll sideways, list filters fold behind a "filters" button, key/value lists stack,
cards narrow. Touch: a long press opens the same context menus (a small script dispatches
`contextmenu`; iOS webviews never send one), and the always-visible row / card checkboxes
are the select mode. Screens hold no layout assumptions beyond the console's classes, so a
completely separate phone UI would be a new shell + stylesheet (and, where needed, new
views) over the same `api` / `state` / `components` -- not a rewrite of the data flow.

## Building

- Web: `scripts/build_gui_web.sh`, which runs `dx build --platform web --release` and
  installs `target/dx/romexplorer-gui/release/web/public` into `app_root_ex/assets/gui_web`
  (served at `/app/`, SPA fallback, token-exempt for the files; the API still needs the
  token). It wipes both directories first, because dx fingerprints every file
  (`app-dxh<hash>.css`) and doesn't clean the old hashes.
- That output is git-ignored, like `app_root_ex/lib/<os>/`: tracking it would commit a
  fresh ~4 MB wasm blob per rebuild and strand the previous hashes. It is not per-OS
  though -- wasm32 is one portable target, so a single build serves every platform, and
  a machine that can't run dx can take the directory from one that can. Without it the
  server still runs and answers `/app/` with "the GUI web build isn't installed".
- Desktop: `dx build --platform desktop` (→ `target/dx/romexplorer-gui/debug/windows/app`).
- `cargo check -p romexplorer-gui` for quick checks (`--target wasm32-unknown-unknown
  --no-default-features --features web` for the web build).
- This machine's quirks: dx can't download its tools through the TLS-intercepting network
  (esbuild and binaryen were fetched with curl into `~/.dx/tools/`), and the real
  `wasm-opt` crashes on this build, so `~/.dx/tools/binaryen-129/bin/wasm-opt.exe` is a
  shim that tries the real one and otherwise copies the file unoptimized. A debug web
  build shows dx's "being rebuilt" overlay (no dev server) -- use `--release`.

## Parity with the console

Every screen above exists in both UIs. "Copy to…", Config › Copy, Config › Users, the
integrity and sheet tools were app-only until 2026-10-03, on the reasoning that the web
build at `/app/` superseded the console; in practice a server unpacked without that build
had no way to create its first account from a browser, so they were ported to
`console.html`. The two are now kept in step by hand: a screen added to one is added to the
other.

The server still serves the web build at `/app/` when `assets/gui_web` is installed. The
console is the server's control panel and is **admin-only**; `/app/` is the client UI and
takes any account.

**Accounts** (server side in `docs/plan/api.md`). The desktop app never signs in: it runs on
this machine's own database. A build talking to another machine (web, mobile) shows **Sign
in** (`screens/users.rs`) -- username and password, or the setup token on a fresh server --
and returns to it on a 401 from any screen. A native build asks the user to confirm the
server's certificate fingerprint on first connect and pins it (`api::rest`); the web build
leaves TLS to the browser and holds its session in an `HttpOnly` cookie, so the page never
has a token and image URLs carry none. **Config › Users** shows who the app is running as,
lets an account change its password and sign out its devices, and lets an admin manage
accounts; creating the admin while on the setup token signs straight in as it. The header
shows the signed-in user; the Edit tab is hidden from non-admins (the server refuses it to
them regardless). Sources are added by signing in to them; only the token they hand back is
kept. A source row's **art** action queues a `media_sync`: the art that source holds and this
machine lacks, fetched from it -- the server is the household's art cache and fetches from
providers what it lacks; this machine calls a provider itself only while the server is
offline (`api::media_sync`; the same happens on its own after a sync that brought new art,
per the `media_sync_after_pull` setting).

The console (`console.html`) signs in with the same cookie, or the setup token, and signs
out. It has its own Config › Users; the last-admin and password rules live in `api::auth`,
which both editors call, so they cannot drift apart. Its sign-in is refused to a non-admin
account by the server, and a regular account that arrives already signed in (the cookie is
shared with `/app/`) is shown an "admin accounts only" card instead of the console.

The desktop app's **Server console** tab shows that same console inside the app for a client
machine: one button (greyed out in standalone mode, or with no main server set), a sign-in
with an account on the server, and then the console in a frame whose requests this process
forwards over the source's pinned connection (`api::assets`, `api::sources::ConsoleSession`).
"Open in browser" is offered beside it.

## Still to do

- Mobile store builds (`dx bundle --platform android|ios`), the persistent cache.
- Mobile images: an `<img>` in the webview loads straight from the server and would reject
  its self-signed certificate. Mobile needs an image proxy through the pinned client, like
  desktop's `/rxasset`.
- The OS keyring for the native token (Windows Credential Manager, Keychain, Secret Service,
  Android Keystore). Today it is `gui.json`, owner-only. Held back until it can be tested on
  each OS, which needs a CI runner per platform.
- Media checks: video / audio through the webview on Linux (WebKitGTK), bundled `pdf.js`.
- Tray icon / run-in-background.
- Server-side convert-before-copy (`PrepareForPickup`).
- Slow list queries are the server's, not the GUI's: Titles / Releases / Contents take
  5-6 s on the full catalog in the console too.

## Verification done

- Every route renders against the live server in the browser pane (web build at `/app/`),
  side by side with the console: titles (cards, selection, context menu with sets and
  keywords), title page (links, media, release order), releases, games, roms, files,
  contents, sets, RA lists and account block, edit (keywords, merges, manual order), jobs,
  config (directories with the folder picker, emulators, storage, settings editors,
  schedules, server), data (providers, DATs, DB).
- Phone width (375 px): bottom tabs, scrolling subtabs, folded filters, card grid.
- Desktop: `dx build --platform desktop` runs, connects to the local server, and renders
  the same Status page in its own window.
