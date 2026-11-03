# romexplorer

**English** · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

A ROM collection manager: it scans your folders, identifies every file by hash against
reference DATs, fetches metadata and art, and converts, verifies and copies the files. It
runs as a server with a browser console and web GUI, or as a desktop app, on Windows, macOS
and Linux. See [docs/release_notes.md](docs/release_notes.md) for what is tested where.

## What it does

- **Scan and identify.** Walks a directory, detects each file's platform from its content,
  hashes it (CRC32, MD5, SHA-1, SHA-256, XXH3, RetroAchievements) and matches it against the
  catalogue by hash first and name second.
- **Read inside containers.** CHD, RVZ/WIA/GCZ, zip and 7z are decoded as streams, so a
  disc image is hashed and identified without being extracted to disk first.
- **Convert and repack.** Create and extract CHD (CD, GD-ROM, DVD) and RVZ; zip and 7z;
  add or strip NES/SNES headers; rebuild a missing `.cue`, `.gdi` or `.m3u`.
- **Catalogue.** Imports Redump, MAME-Redump, No-Intro, TOSEC, RetroAchievements and
  RetroBIOS DATs into a bundled MariaDB, and merges regional releases and titles.
- **Metadata and art.** IGDB, ScreenScraper, TheGamesDB, SteamGridDB, MobyGames, Hasheous,
  PlayMatch, EmuReady and RetroAchievements, each rate-limited on its own.
- **Copy to a device.** Copies selected games to a folder or card, converting on the way,
  with names and layouts a frontend expects.
- **Verify and back up.** Integrity checks against recorded hashes; zstd backups of the
  database and catalogue; restore.
- **Share between machines.** A main server's catalogue and art can be mirrored by clients
  and peers; a SQLite export feeds a phone.

Everything is reachable through the REST API (`/api/v1/...`), the browser console (`/`) and
the web GUI (`/app/`).

## Initial setup

There are three packages. Pick one:

| package | what it is | setup |
|---|---|---|
| **server** | server, worker, bundled MariaDB, web GUI | run the installer |
| **desktop** | GUI app with its own MariaDB | just launch it |
| **cli** | chdlite: standalone convert / extract / hash tools, no database | just run it |

### Server

1. Unpack the package.
2. Optional: copy `romexplorer.conf.example` to `romexplorer.conf` and edit it. Every value
   is optional; a blank password is generated. This file is read on the first install only.
3. Run the installer:
   - Linux / macOS: `./install.sh` -- as root it installs to `/opt/romexplorer` as a system
     service; as an ordinary user to `~/.local/opt/romexplorer` as a user service.
   - Windows: `.\install.ps1`, from an elevated PowerShell.
4. The first start creates the database, its accounts and the admin login, then prints the
   console login **once**. Keep it.
5. Open `http://127.0.0.1:7337/` and sign in.

Defaults: the server listens on `127.0.0.1:7337` and the bundled MariaDB on `3307`. Set
`server_bind=0.0.0.0` to serve the LAN; other machines are then served over TLS with a
certificate the server makes on first start.

Afterwards:

- Change the admin password in the console, or with `romexplorer_server reset-admin`.
- Check an install with `romexplorer_dbtool check`: it lists the native libraries and
  the database it found.
- Remove it with `./install.sh --uninstall` (keeps data) or `--uninstall --purge`.

### Updating the server

Run the installer from the new release over the existing install. It sees the database,
stops the service, replaces the executables, `lib/` and the shipped assets, and starts it
again. Your database, settings, credentials, media and everything else under `data/` and
`assets/` are kept.

- Linux / macOS, straight from the newest release:
  `curl -fsSL https://raw.githubusercontent.com/DesertDoggy/romexplorer/main/scripts/install.sh | bash -s -- --release latest`
- Or unpack the new package and run its `./install.sh`, or point the installed one at it:
  `./install.sh --from romexplorer-server-<version>-<os>-<arch>.tar.gz`
- Windows: unpack the new package and run its `.\install.ps1` from an elevated
  PowerShell, or `.\install.ps1 -From <package>.zip`.

Run it as the same user as the first install (root or not), and pass the same `--prefix` /
`-Prefix` if you chose one. While romexplorer is in alpha the database schema may change
between releases, so take a backup first (Config › Backup).

### Desktop

Unpack and run `romexplorer` (on macOS, `romexplorer.app`; it is not notarized, so the first
launch needs right-click > Open). The first launch creates and starts its own database.

### Command-line tools (chdlite)

Drop a disc image, get a CHD or RVZ. `chdlite` (fast), `chdcomp` (smallest), `chdread`
(extract) and `chdhash` (hash) are one tool per mode, so a file dropped onto the executable
does the right thing; `chdlite-gui` is the same in a window. Every option is also a flag
(`--help`). chdlite is released separately: see
[docs/chdlite_release_notes.md](docs/chdlite_release_notes.md).

### Provider keys

Optional. Enter API keys in Config › Provider credentials as an administrator. They are
stored encrypted on the server and never shown again; a provider without keys is simply
unavailable.

### First use

1. Add a directory and say which platform it holds, or let detection decide.
2. Run a DAT sync so there is a catalogue to match against.
3. Run a file scan on the directory.
4. Run a hash match, then provider queries for metadata and art.

Each of those is a job; see below.

### From source

```bash
git submodule update --init --recursive
bash scripts/build_native_deps.sh        # every native library, into app_root_ex/lib/<os>/<arch>/
bash scripts/build_gui_web.sh            # the web GUI (needs `dx`)
cargo run --bin romexplorer_server
bash scripts/build_release.sh            # or: all packages for this host, into release/<version>/
```

`build_native_deps.sh --check` reports missing tools without building. On macOS it needs
Homebrew's `bash`, `bison` and `ninja`.

## Jobs

Anything slow runs as a background **job**, so it keeps running with no browser open. Each
shows its progress, can be cancelled, and can be put on a schedule (the Scheduled jobs tab) to
recur every so many minutes, hours, days, weeks or months. The main ones:

- **DAT sync** -- download and import the reference DATs (Redump, No-Intro, TOSEC, ...).
- **Provider metadata and media download** -- fetch game info and art from IGDB,
  ScreenScraper, SteamGridDB and the others.
- **File scan** -- walk a directory and hash its files, then match them against the catalogue.
- **Integrity check** -- re-read held files and compare them with their recorded hashes.
- **Sheet / playlist check and creation** -- check that `.cue`, `.gdi` and `.m3u` files
  still resolve, and rebuild missing ones.

## Third-party code

romexplorer is built on these projects. Most are used as libraries, pinned as submodules
under `external/`. "Patched" means a few lines of the project's own code are changed at build
time; "wrapper" means we add our own small library around it without editing its code. Both
live in `deps/<name>/`, never in the submodule. Thanks to all of them.

| project | used for | our changes |
|---|---|---|
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | zip / 7z | patched, wrapper |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | CHD create / extract | patched, wrapper |
| [MAME](https://github.com/mamedev/mame) | the source chdman-simd builds from | none |
| [libchdr](https://github.com/rtissera/libchdr) | its LZMA decoder; reference for the CHD reader | none |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | GameCube / Wii (RVZ, WIA, GCZ) | wrapper |
| [Cemu](https://github.com/cemu-project/Cemu) | Wii U .wua | wrapper |
| [ZArchive](https://github.com/Exzap/ZArchive) | .wua / .zar writing | none |
| [zstd](https://github.com/facebook/zstd) | compression | none |
| [fmt](https://github.com/fmtlib/fmt) | used by Cemu's code | none |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | Xbox XISO | wrapper |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | PS1 EBOOT.PBP | wrapper |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | RetroAchievements hashes | patched, wrapper |
| [MariaDB Server](https://github.com/MariaDB/server) | the bundled database | build file patched |
| [MariaDB Connector/C](https://github.com/mariadb-corporation/mariadb-connector-c) | database client | none |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | Deflate | none |
| [OpenSSL](https://github.com/openssl/openssl) | TLS | none |
| [libxaac](https://github.com/ittiam-systems/libxaac) | AAC audio | wrapper |
| [PlayMatch](https://github.com/RetroRealm/playmatch) | provider clients; its name matching ported | code copied in and modified |
| [RomM](https://github.com/rommapp/romm) | reference, platform icons | not used as code |
| [RAChecker](https://github.com/x3kim/RAChecker) | reference for the RetroAchievements client | not used as code |
| [psx-covers](https://github.com/xlenore/psx-covers), [ps2-covers](https://github.com/xlenore/ps2-covers) (xlenore) | PS1 / PS2 cover art, by serial | downloaded when used, not bundled |

Build details, pins and patches are in [deps/README.md](deps/README.md). Each project's
license ships with the binaries in `lib/LICENSES/`.
