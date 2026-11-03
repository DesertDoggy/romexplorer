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
- **Copy to a device.** Copies games and their art from the server or this machine to a
  folder or card, converting on the way, with names and layouts a frontend expects; art is laid
  out for RetroArch or PCSX2 / DuckStation.
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

Optional. Enter API keys in Config › Provider credentials as an administrator. A provider
without keys is simply unavailable.

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

## Scan and hash match

**File scan** walks a folder and hashes every file (CRC32, MD5, SHA-1, SHA-256, XXH3 and the
RA hash). Archives (CHD, RVZ/WIA/GCZ, zip, 7z) are hashed from the inside. When the scan ends,
it queues a hash match of the same folder with the boxes you ticked. With all boxes off, no
match runs.

| box | what it does |
|---|---|
| DAT match | Links each game to the DAT release whose hashes match its files (a CHD also by its tracks), then by serial for what no hash matched. Offline. |
| RetroAchievements | Links games to RetroAchievements games by their RA hash, from the RA data already synced. Offline, always on. |
| Hasheous | Looks each file up by hash on hasheous.org (no key). Returns the release and its ids on other providers. |
| PlayMatch | Looks up files that still have no IGDB id (no key), 100 per request. Returns the same kind of ids. |

Hasheous and PlayMatch go online only when the data is not cached locally.

**Providers Hasheous and PlayMatch lead to:** IGDB, TheGamesDB, RetroAchievements,
SteamGridDB, ScreenScraper and MobyGames (data and art, by the ids found), plus a name search on
IGDB, TheGamesDB, SteamGridDB, MobyGames, EmuReady and ScreenScraper for titles still without a
link. Providers without keys are skipped.

## Copy

**Copy games and their artwork to another destination.** In desktop client mode you can also
copy from the main server: add the network-mounted directory (SMB, NFS, ...) and the server's
collection root in the client settings.

**You can copy a set, or individual titles, releases, crossplats and defs.**

Copy modes (chosen when you copy):

| mode | for | rules |
|---|---|---|
| To this collection | server → a collection folder on this machine | skips what this machine already holds; the copies count as held here straight away |
| To an SD card / USB / phone folder | any folder outside the collection | nothing is skipped for being held elsewhere; nothing is recorded |
| Within this collection | converting in place (ISO → RVZ, ...) | keeps both copies |
| Within the server's collection | from the server's own UI | every file verified; never overwrites |
| Upload | this machine → the server | only what the server lacks; never overwrites; the server rescans it |

- Copies go into one subfolder per platform. You choose the subfolder name style when you copy.
- Files can be converted on the way, to archive them and save space, or into another format a
  device needs.
- If the destination file already exists, it is skipped.

**Artwork can also be exported.** Tick "export art afterwards" in the copy dialog, or use
**export art...** on its own. Image files are renamed and laid out for emulators:

- **RetroArch:** `<system>/Named_Boxarts`, `Named_Titles`, `Named_Snaps`, `Named_Logos`,
  `<rom name>.png`, the libretro-thumbnails layout.
- **PCSX2 / DuckStation:** one covers folder, `<serial>.<ext>`, or named after the game file.

Which provider's picture fills each slot follows `media_export_priority` (real box scans first).

## Collection tab

The collection is the database of all the games you own, and of the other games that exist in
databases such as Redump, No-Intro, TOSEC and IGDB (DAT / metadata download required).

| sub-tab | one row per |
|---|---|
| Titles\* | A Title is a single game on a single platform. |
| Releases\*\* | A Release is a single game where each region, language, revision, edition, demo etc. counts separately. |
| Crossplats\*\*\* | A Crossplat combines all Titles of the same game on different platforms. |
| Defs\*\*\*\* | A Def is for choosing one version of a game from all its versions (a game family): the original plus its remasters, ports, remakes, GOTY editions etc. One row per game family. |
| Games | A Game is a complete (or incomplete) set of roms for one game. If you hold several copies, each counts as one Game. A game can contain several roms (Disc 1, Disc 2, ...). |
| Roms | A Rom is a single unit of game media. Separate discs count as separate roms. A rom can contain several files (Disc 1 = cue + track 1.bin + track 2.bin). |
| Files | Individual files on disk. |
| Contents | A Content is a set of bytes, by hash. It can be a file, or a single entry inside an archive. A content can also contain contents. |
| Sets | A Set is your own ordered list of Titles / Releases / Crossplats / Defs. |
| Multimedia, Documents, Source code, Etc | Video and music discs; manuals, magazines and scans; source code; tools, trainers, saves. |
| Patches | RetroAchievements compatibility patches (with download), DAT-listed patches, your patch folders. |
| BIOS / firmware | The machine's system files: BIOS, firmware, NAND and SD card images. |
| Audio | Your audio files in the audio directory. These (and titles) can be tagged, so a soundtrack plays while you view the Title page. |

\* The same game on a different platform (Hoge Quest (PS2), Hoge Quest (Dreamcast)) counts
as a separate Title. Different revisions (Hoge Quest, Hoge Quest (Rev A)), regions
(Hoge Quest (Japan), Adventure of Hoge (USA)) or languages (Hoge Quest (ja), Hoge Quest (fr))
are bundled into the same Title.

\*\* On copy, if several copies of the same release are found, the complete one (no missing
files) wins, then the priority set in settings (headered > unheadered, z64 > n64, etc.).
On copy, one release is copied from the server or the client, chosen by your
region / language / edition (regular, budget, special release) priorities. Releases you do not
own are skipped for the next priority.

\*\*\* Hoge Quest includes Hoge Quest (PS2) and Hoge Quest (Dreamcast).

\*\*\*\* For when you want to play only the latest version, or the one with the most playable
content. By default a Def picks the latest Crossplat, but some Defs carry ranked notes for
cases where the latest Crossplat misses content from earlier versions (e.g. Hoge Quest 3D
Remake (PC) lacks the extra dungeon of Hoge Quest (GBA)). You can drag the versions into the
order you prefer. Since the definitive version may only exist on current platforms, you can
choose to copy the top-priority crossplat / title in your collection, or to copy nothing when
the most definitive version is not in your collection (i.e. go play it on Steam).

Note: Crossplats and Defs are built from IGDB metadata, so it must be downloaded. A setting can
download the related data automatically (off by default, because the large database slows
everything down).

## RetroAchievements tab

Your RetroAchievements account and the games with achievements, as of the last account sync
(needs `ra_username` and `ra_api_key`). The account block on top shows your profile and
progress. A game's page lists its achievements and its compatibility patches, with a download
button.

If you copy a Title or add it to a Set from the RA tab, **only** releases that have
achievements are chosen. (E.g. if Hoge Quest (USA) has achievements and Hoge Quest (Japan)
doesn't, Hoge Quest (USA) is copied even if your region priority is Japan > USA.)

| sub-tab | shows |
|---|---|
| Titles | Titles with RetroAchievements achievements. |
| Mastery | Titles where you unlocked every achievement in hardcore mode. |
| Complete | Titles where you unlocked every achievement in softcore mode. |
| Beat | Games you beat but have not completed. |
| In progress | Started but not mastered or completed (beaten ones too, unless the `ra_include_beaten` setting is off). |
| To be played | Titles with achievements that you haven't started. |

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
