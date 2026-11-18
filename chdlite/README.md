# chdlite

Drop a disc image (CUE + BIN, GDI + BIN, ISO) on chdlite and it becomes a CHD or RVZ; drop a
`.chd` or `.rvz` and you get the CUE + BIN, GDI + BIN or ISO back. It also reads metadata and detects the platform, hashes the content inside CHD, RVZ, CSO and PBP, puts NES, N64 and
SNES dumps into the layout you choose, and decrypts 3DS titles. No database or setup is needed.
It uses the romexplorer core and is released on its own (version 0.3.4).

**English** · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**Download:** [CHD Lite releases](https://github.com/DesertDoggy/romexplorer/releases?q=c0) · **Changes:** [release notes](../docs/chdlite_release_notes.md) · Part of [romexplorer](../README.md)

> **chdlite does not distribute or download ROMs, BIOS files or keys, and does not support
> piracy.** Use it with games you own and dumped yourself.

## Window

Open `chdlite-gui` (on macOS `chdlite.app`, on Linux the chdlite AppImage). It has four modes:

| Mode | Description |
|---|---|
| **Lite** | Compresses fast. A `.chd` or `.rvz` is extracted to CUE + BIN, GDI + BIN or ISO. |
| **Comp** | Compresses for the smallest file, slowly. A `.chd` or `.rvz` is extracted the same way. |
| **Read** | Reads the metadata and detects the platform. |
| **Hash** | Hashes the content inside a file. |

Pick a mode, then drop files or folders on the window (or choose them). **Settings** holds the
rest: threads and files at once, the output folder and replacing existing outputs, the Lite
preset and Comp codecs, RVZ codec, level and block size, hashes, the cue style and GD-ROM
sheet, the layout cartridge dumps and 3DS titles are written in, and whether to ask before a
lossy output (WUA, ZAR, XISO). The window remembers them, and the command line uses the same
settings.

**Android.** The chdlite app is the same window. Files and folders are chosen in its own
browser.

**macOS and Linux.** The Mac app is not notarized, so its first launch needs right-click > Open.

## Conversion

The output is chosen from the image.

**Compress**

| Input | Output |
|---|---|
| CD: CUE + BIN, TOC + BIN, or a lone BIN/IMG track | CD CHD |
| GD-ROM (Dreamcast): GDI + BIN/RAW | GD-ROM CHD |
| DVD (PS2, PSP and others): ISO | DVD CHD |
| PSP: CSO | DVD CHD |
| PS1: PBP | CD CHD |
| GameCube, Wii: ISO, GCM | RVZ |
| Wii U: WUD, WUX | WUA |
| Xbox 360: ISO | ZAR |
| Xbox: ISO | XISO |

**Extract**

| Input | Output |
|---|---|
| CD CHD | CUE + BIN (one BIN per track) |
| GD-ROM CHD | GDI + BIN, or CUE + BIN with `--gdrom-sheet cue` |
| DVD CHD | ISO |
| RVZ | ISO |
| CSO | ISO |
| PBP | CUE + BIN |

WUA, ZAR and XISO keep the game's files but not the disc itself, so they can't be turned back into
the original image. Only lossless inputs are taken: not WUA, ZAR or XISO. A Wii U disc
needs its key beside it.

**Cartridge dumps** (`.nes`, `.unh`, `.z64`, `.v64`, `.n64`, `.sfc`, `.smc`, `.swc`) are not
compressed. They are written in the layout set for their platform: NES with or without the
header (or as `.unh`), N64 in one of its three byte orders, SNES with or without the copier
header. Whether a NES dump needs a header is looked up by its hash, so only known games are
converted.

**3DS titles** (`.cia`, `.3ds`, `.cci`, `.cxi`, `.3dsx` and the compressed `.zcci`, `.zcia`,
`.zcxi`, `.z3dsx`) are decrypted: a game becomes `.cci`/`.3ds`, `.cxi`, or compressed `.zcci`/
`.zcxi`; updates, DLC and demos stay a `.cia` (`.zcia` compressed). No keys come with chdlite.

**Hashes.** SHA-1, MD5, CRC32, SHA-256, XXH3 and the RetroAchievements hash, which also says
whether RetroAchievements has achievements for the game (from the bundled list).

## Read

Read detects the platform from the disc's own data and reads the game's serial and title from
it. It also shows:

| File | Shown |
|---|---|
| CHD | CHD version and type (CD, GD-ROM, DVD, hard disk); platform, serial and title; full size, file size and compression ratio; hunk size and count; unit size; codecs; SHA-1 of the whole and of the data; the parent's SHA-1 (if any); each track's type and length |
| RVZ | GameCube or Wii; game ID and title; disc size, file size and ratio; block size |
| CSO | CSO version and blocks; platform, serial and title; ISO size, file size and ratio |
| PBP | Number of discs; platform, serial and title; each disc's tracks and size |
| CUE + BIN, GDI + BIN, ISO | Platform, serial and title; output format |
| Wii U WUD, WUX | Disc size; title ID and version |
| 3DS titles | Container (CIA, cartridge image, NCCH, 3DSX); compressed or not; title type; encrypted or not; output format |
| Cartridge dumps | Platform and size; current layout (header, byte order); output format |

## Drag and drop

Each tool has one job, so dropping files on its executable does that job, with the saved settings.

| Tool | Action on drop |
|---|---|
| `chdlite` | Compressed fast. A `.chd` or `.rvz` is extracted to CUE + BIN, GDI + BIN or ISO. |
| `chdcomp` | Compressed for the smallest file. A `.chd` or `.rvz` is extracted to CUE + BIN, GDI + BIN or ISO. |
| `chdread` | Reads the metadata and detects the platform. |
| `chdhash` | Hashes the content inside a `.chd`, `.rvz`, `.cso` or `.pbp`, or the file itself. |

## Command line

Every tool takes every command and option. Inside the apps the tools are in
`chdlite.app/Contents/MacOS/`, or run as `chdlite.AppImage chdlite ...`.

```
chdlite [command] <file|folder>... [options]
```

| Command | Description |
|---|---|
| `auto` | `.chd`/`.rvz` are extracted, anything else is created with the fast preset. |
| `create` | Create with the fast preset. |
| `comp` | Create for the smallest file. |
| `extract` | Extract `.chd`, `.rvz`, `.cso` and `.pbp` to CUE + BIN, GDI + BIN or ISO, and compressed 3DS titles. |
| `read` | Read the metadata and detect the platform. |
| `hash` | Hash the content. |

| Option | Description |
|---|---|
| `-o`, `--output <dir>` | Write here instead of beside each input. |
| `-f`, `--force` | Replace outputs that already exist. |
| `-np`, `--threads <n>` | Threads in total (default: the physical cores less 4). |
| `-jf`, `--concurrent-files <n>` | Files converted at once. |
| `--preset pc\|mobile` | The fast preset `create` uses. |
| `--comp best\|chdman\|custom` | `comp` mode. |
| `-c`, `--compression <list>` | CHD codecs, up to 4. |
| `-hs`, `--hunksize <bytes>` | CHD hunk size. |
| `--rvz-codec`, `--rvz-level`, `--rvz-block`, `--rvz-scrub` | RVZ settings. `--rvz-scrub` drops junk data and can't be undone. |
| `--hash <list>` | `sha1,md5,crc32,sha256,xxh3,rahash` (default `sha1,rahash`). |
| `--cue-style <style>` | The extracted cue sheet's dialect: `redump`, `redump_catalog` or `chdman`. |
| `--gdrom-sheet gdi\|cue` | An extracted GD-ROM's sheet. |
| `--nes`, `--n64`, `--snes`, `--3ds <format>` | The layout cartridge dumps and 3DS titles are written in. |
| `-h`, `--help` / `-V`, `--version` | Help and version. |

Defaults come from `chdlite.toml`, beside the program or in your settings folder (`--help`
shows where).

## Bundled libraries

| Library | Licence |
|---|---|
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | [BSD-2-Clause](../app_root_ex/lib/LICENSES/chdman-simd.LICENSE) |
| [MAME](https://github.com/mamedev/mame) | [GPL-2.0+](../app_root_ex/lib/LICENSES/mame.LICENSE) |
| [libchdr](https://github.com/rtissera/libchdr) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/libchdr.LICENSE) |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | [GPL-2.0+](../app_root_ex/lib/LICENSES/dolphin.LICENSE) |
| [Cemu](https://github.com/cemu-project/Cemu) | [MPL-2.0](../app_root_ex/lib/LICENSES/cemu.LICENSE) |
| [ZArchive](https://github.com/Exzap/ZArchive) | [MIT-0](../app_root_ex/lib/LICENSES/ZArchive.LICENSE) |
| [zstd](https://github.com/facebook/zstd) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/zstd.LICENSE) |
| [fmt](https://github.com/fmtlib/fmt) | [MIT](../app_root_ex/lib/LICENSES/fmt.LICENSE) |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | [BSD](../app_root_ex/lib/LICENSES/extract-xiso.LICENSE) |
| [Beetle PSX](https://github.com/libretro/beetle-psx-libretro) | [GPL-2.0](../app_root_ex/lib/LICENSES/beetle-psx-libretro.LICENSE) |
| [RALibretro](https://github.com/RetroAchievements/RALibretro) | [GPL-3.0](../app_root_ex/lib/LICENSES/RALibretro.LICENSE) |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | [zlib](../app_root_ex/lib/LICENSES/zlib-ng.LICENSE) |
| [OpenSSL](https://github.com/openssl/openssl) | [Apache-2.0](../app_root_ex/lib/LICENSES/openssl.LICENSE) |

## Licence

[AGPL-3.0](../LICENSE), like romexplorer.
