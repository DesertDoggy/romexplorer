# Handhelds and phones: Big Picture mode and romcompanion

**Status (2026-11-11): phase 1 implemented** (Big Picture mode in the app); **phase 2 started**
(romcompanion for Android: sync and browsing, no launching yet). The full plan,
with the research behind it, is the doc "Big Picture mode and romcompanion: handheld and
mobile plan" (https://claude.ai/artifact/PfRVKjVML7G1mZWYwXSxCP); this file is its summary
in the repo.

## One UI, two packages

- **Big Picture mode**, inside the main app, on Windows and Linux handhelds: x86 (Steam Deck,
  ROG Ally, Legion Go, Windows UMPCs) and later ARM consoles running Linux. These run the
  app with its own database, copy job, emulator settings and launcher, so Big Picture is a
  view, not a second app.
- **romcompanion**, on Android (Retroid Pocket, AYN Thor are the test devices) and later iOS,
  which cannot run the app (no MariaDB, see `gui.md`). It is the same UI over a SQLite copy
  of the server's data (`api::sqlite`), read-only, with a server download route for getting
  ROMs onto the phone.

## What Big Picture mode is (phase 1, `romexplorer-gui/src/screens/bigpicture.rs`)

- Routes `/bigpicture/list/:tab`, `/bigpicture/cross/:igdb`, `/bigpicture/title/:id`,
  `/bigpicture/release/:id`, in their own full-screen frame. Entered from the header's
  **Big Picture** button, or at start-up when the device is set to (stored in the webview's
  `localStorage`, with the layout: per device, not a server setting).
- Tabs: Cross-platform, Titles, Releases, Games, Files, and the RetroAchievements lists.
- Layout: **Auto** (one column in portrait, one row in landscape), **Single**, **Grid**. The
  focused item's details fill the room the covers leave; a grid shows them on the right from
  900 px wide.
- One **Filter** button: a sheet of filter types (platform, held), then their values, then
  Apply. Search on its own button.
- Pages show the related items as rows of covers: a title's releases, patched releases,
  related titles and extras; a release's other releases and patched releases. Each has Start
  and a menu.
- Menu (Y, long press, right click): Start, Open with…, Start disc N / Open disc N with…,
  Open page, Open in the app. Open with… lists the platform's emulators with the default
  marked, and can make another the default (it saves the emulator, as Config › Emulators
  does).
- Start uses the platform's default emulator (`POST /api/v1/launch` without an emulator).
  A game with an `.m3u` opens it only when the emulator's new **opens .m3u playlists**
  option is on, else its first disc (`api::launch::Resolved::file_for`).
- Controller: D-pad / stick moves the focus (spatial, in JS), A opens, B back, X starts,
  Y menu, LB / RB tabs, Select filter, Start search. Read through the webview's Gamepad API;
  the keyboard has the same keys.

## Parity

Big Picture mode and romcompanion are app-only by design; CLAUDE.md records the exception.
The emulator option they rely on (opens .m3u playlists) is in both UIs.

## Next phases

1. ~~Big Picture mode, x86 handhelds.~~ Done; still to check on a Steam Deck and a Windows UMPC.
2. romcompanion for Android (started, see below): shared handheld UI crate over a data trait (REST / SQLite), a
   `companion` cache kind with the `remote_*` mirror (minus `remote_sources.token`) so a
   phone synced from a client can filter by where an item is held (main server, each shared
   client, this device); a server download route with `Range` (its content types added to
   the compression predicate); SAF ROM folder; intent presets checked on a device; a warning
   above 4 GB or under 1 GB left free.
3. Big Picture mode on ARM Linux consoles: a renderer without WebKitGTK (Dioxus native
   renderer spike), a lean MariaDB setup (about 2 GB RAM minimum).
4. romcompanion for iOS (later): share sheet / URL-scheme launching.

## romcompanion (phase 2, `romcompanion/`)

A slim package of the app: the same `romexplorer-gui` sources built with the `mobile`
feature, so the code grows into a full Android romexplorer later by turning features on, not
by forking. The phone opens straight into Big Picture mode. No scans, converts, media
downloads of its own or ROM music; Start is hidden until launching is done.

- **Sync** (`POST /api/v1/mobile/sync`, a SqliteExport job of kind `mobile`, `api::mobile`):
  the phone picks the machines that count as held (this server and each shared client), the
  scope (what they hold, or also the catalogue's titles not owned yet on picked platforms),
  the tabs, title/release pages, covers and their format. `POST /api/v1/mobile/estimate`
  gives the size first. The file is downloaded through `/api/v1/cache/{name}` and swapped
  in whole.
- **The file**: the MariaDB tables with the same names and columns, slimmed to the scope
  (`remote_sources.token` nulled), so a full Android build can widen it; plus `_mobile_*`
  tables holding the answers to the routes the phone asks (lists, pages, RA, covers as
  server-made AVIF or JPEG thumbnails). `api::offline` answers the same route strings from
  it; `CompanionBackend` asks the server first and falls back to the copy when it is away.
- **Defaults**: Cross-platform, Definitive, Titles, Releases and RA tabs on; Games, Files
  and Contents off. Covers on, audio off (not synced yet). Auto sync daily when the server
  answers.
- **RA progress** syncs on its own (`GET /api/v1/mobile/ra`). Its auto sync is off by
  default; turning it on warns that it may use mobile data.
- Provider and media downloads can be started from an item's menu, on the server, by hand
  only; the phone never starts them on its own.
- Build: `scripts/build_romcompanion_android.sh` (as chdlite's). iOS builds from the same
  crate with `dx bundle --ios`, untested.
- Not done yet: launching, the ROM download route and SAF folder, audio sync, its own icon.
