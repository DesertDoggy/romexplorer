# rommusic

Drop a ROM or disc image on `rommusic` and it writes the game's music as FLAC.
It uses the same readers and players the romexplorer app plays ROM music with, and is
released on its own (version 0.2.1).

**English** · [日本語](README.ja.md) · [Français](README.fr.md) · [Español](README.es.md) · [Italiano](README.it.md) · [Português](README.pt.md) · [Deutsch](README.de.md) · [한국어](README.ko.md) · [Polski](README.pl.md)

**Download:** [Rom Music releases](https://github.com/DesertDoggy/romexplorer/releases?q=m0) · **Changes:** [release notes](../docs/rommusic_release_notes.md) · Part of [romexplorer](../README.md)

> **rommusic does not distribute or download ROMs, BIOS files or keys, and does not support
> piracy.** Use it with games you own and dumped yourself.

## Window

Open `rommusic-gui` (on macOS `rommusic.app`, on Linux the rommusic AppImage):

1. Choose files or a folder, or drop them on the window. ROMs, disc images and dumped game
   folders all work.
2. Pick **FLAC**, **Original audio** or **Both**.
3. Pick an output folder, or leave it empty to write beside each input.
4. Press **Start**.

**Options** holds the rest: jingles, sound effects and movie sound, a track limit, the sample
rate and bits, and the SNES run (how long the game runs, and pressing Start while it runs). The
window remembers them.

**Android.** The rommusic app (Android 9 or later) is the same window. It asks for All files
access on first start, so it can read your ROMs and write the music; files and folders are
chosen in its own browser, and a run keeps going with the screen off.

**macOS and Linux.** `rommusic.AppImage --install` adds the window to the desktop menu. The Mac
app is not notarized, so its first launch needs right-click > Open.

## Supported games

| Platform | Files | Music |
|---|---|---|
| CD games: PlayStation, Saturn, Mega-CD, PC Engine CD, Neo Geo CD, Dreamcast and others | CHD, CUE + BIN, GDI + BIN, ISO | CD audio tracks; music files on the disc (ADX, XA, AT3 and others, also inside CPK and AFS archives) |
| PlayStation 2, PSP, GameCube, Wii, Xbox, Xbox 360 and others | CHD, ISO, RVZ, WIA, GCZ, WBFS, CSO, ZSO, XISO, dumped game folders | Music files on the disc (ADX, AT3, ATRAC3, WMA, XMA, BRSTM, HCA and others, also inside CPK, AFS, ARC and SZS archives) |
| Wii U | WUA; WUD, WUX or a title folder with its key beside it | Music files on the disc (BFSTM and others) |
| Nintendo DS | NDS | SDAT sound archive (STRM streams, SSEQ sequences) |
| Nintendo 3DS | 3DS, CCI (decrypted only) | Music files in the sound archive (BCSTM and others) |
| NES / Famicom, Famicom Disk System, Game Boy, Game Boy Color, Game Boy Advance, Virtual Boy, SG-1000, Master System, Game Gear, Mega Drive / Genesis, PC Engine / TurboGrafx-16, SuperGrafx, MSX, WonderSwan, WonderSwan Color, Neo Geo Pocket, Neo Geo Pocket Color | ROM, also in zip or 7z | BGM binary code |
| SNES / Super Famicom | ROM, also in zip or 7z | BGM binary code |
| Nintendo 64 | ROM, also in zip or 7z | BGM binary code |
| PC-98 | Floppy images (FDI, HDM, FDD and others), also in zip or 7z | PMD and FMP music files on the floppy |

Even on a supported platform, not every game is supported, for technical reasons. Disc images inside zip or 7z, encrypted images, PS3, Vita and downloaded packages
are not read.

## Output

For each input, rommusic makes a folder named `<name> (music)` next to it (or in the output
folder). An earlier run's files there are replaced.

| Output | Contents |
|---|---|
| **FLAC** (default) | `01 - <track name>.flac`, `02 - ...`: the background music. A looping track plays its loop twice and fades out. `index.txt` lists each track. |
| **Original audio** | The music files as the game stores them, in an `original` folder. CD audio tracks and cartridge music have none; use FLAC for these. |
| **Both** | FLAC and the original files. |

Original files hold the game's own code and data. Keep them with your ROMs and don't share
them.

## Drag and drop

Drop one or more files or folders on `rommusic.exe` (or `rommusic` on Linux and macOS). It
writes FLAC with the default settings, the same as **Start** in the window with nothing
changed.

## Command line

The command-line tool is `rommusic` (inside the apps: `rommusic.app/Contents/MacOS/rommusic`,
or `rommusic.AppImage rommusic ...`). Every window setting is a flag.

```
rommusic [options] <ROM, disc image or folder>...
```

Examples:

```
rommusic "Hoge Quest (Japan).chd"
rommusic --all --max 20 "Hoge Quest (Japan).chd"
rommusic --out D:\music "Hoge Quest (Japan).nes"
rommusic --hires "Hoge Quest (Japan).cue"
rommusic --format both "Hoge Quest (Japan).sfc"
```

| Option | Description |
|---|---|
| `--all` | Also jingles, sound effects and the sound of movies. By default only background music is written. |
| `--max N` | At most N tracks from each input. |
| `--out DIR` | Put the `<name> (music)` folders in `DIR` instead of beside each input. |
| `--rate native` | The default: each track at the rate it is decoded at. |
| `--cd` / `--rate cd` | 44.1 kHz, 16-bit. |
| `--dvd` / `--rate dvd` | 48 kHz, 24-bit. |
| `--hires` / `--rate hires` | 96 kHz, 24-bit. |
| `--rate <Hz>` | Any rate from 8000 to 192000. |
| `--bits 16\|24` | Bits per sample (16 by default, 24 with `--dvd` and `--hires`). |
| `--format flac` | The default: FLAC only. |
| `--format original` | The music as the game stores it, in an `original` folder, and no FLAC. |
| `--format both` | FLAC and the original files. |
| `--seconds N` | SNES: how long the game runs before its music is taken (default 20). |
| `--start` | SNES: press Start now and then while the game runs, for games whose music starts after the title screen. |
| `--list` | Only list the tracks; write nothing. A looping track shows one pass and "loop"; its file plays the loop twice and fades. |
| `--verbose` | Say why a file or track was skipped. |
| `--no-pause` | Don't wait for Enter at the end. It only waits in the window that opens for a dropped file. |
| `-h`, `--help` / `-V`, `--version` | Help and version. |

A rate other than `native` is resampled. Resampling does not add detail; it is for players
that want every file at the same rate, or that handle unusual rates such as 37.8 kHz poorly.
A resampled file says so in its comment tag.

## Bundled libraries

| Library | Licence |
|---|---|
| [game-music-emu](https://github.com/libgme/game-music-emu) | [LGPL-2.1+](../app_root_ex/lib/LICENSES/gme.LICENSE) |
| [Game_Music_Emu (kode54)](https://github.com/kode54/Game_Music_Emu) | [LGPL-2.1+](../app_root_ex/lib/LICENSES/gme-kode54.LICENSE) |
| [agbplay](https://github.com/ipatix/agbplay) | [LGPL-3.0](../app_root_ex/lib/LICENSES/agbplay.LICENSE) |
| [SSEQPlayer](https://github.com/kode54/SSEQPlayer) | [WTFPL](../app_root_ex/lib/LICENSES/SSEQPlayer.LICENSE) |
| [lazyusf2](https://gitlab.com/kode54/lazyusf2) | [GPL-2.0+](../app_root_ex/lib/LICENSES/lazyusf2.LICENSE) |
| [Highly Experimental](https://gitlab.com/kode54/highly_experimental) | [GPL-3.0](../app_root_ex/lib/LICENSES/highly_experimental.LICENSE) |
| [Highly Theoretical](https://gitlab.com/kode54/highly_theoretical) | [GPL-3.0](../app_root_ex/lib/LICENSES/highly_theoretical.LICENSE) |
| [mGBA](https://github.com/mgba-emu/mgba) | [MPL-2.0](../app_root_ex/lib/LICENSES/mgba.LICENSE) |
| [Beetle WonderSwan](https://github.com/libretro/beetle-wswan-libretro) | [GPL-2.0+](../app_root_ex/lib/LICENSES/beetle-wswan.LICENSE) |
| [libvgm](https://github.com/ValleyBell/libvgm) | [BSD-3-Clause, GPL-2.0+](../app_root_ex/lib/LICENSES/libvgm.LICENSE) |
| [VGMTrans](https://github.com/vgmtrans/vgmtrans) | [zlib](../app_root_ex/lib/LICENSES/vgmtrans.LICENSE) |
| [TinySoundFont](https://github.com/schellingb/TinySoundFont) | [MIT](../app_root_ex/lib/LICENSES/TinySoundFont.LICENSE) |
| [98fmplayer](https://github.com/myon98/98fmplayer) | [BSD-2-Clause](https://github.com/myon98/98fmplayer) |
| [vgmstream](https://github.com/vgmstream/vgmstream) | [ISC](https://github.com/vgmstream/vgmstream/blob/master/COPYING) |
| [FFmpeg](https://github.com/FFmpeg/FFmpeg) | [LGPL-2.1+](https://github.com/FFmpeg/FFmpeg/blob/master/LICENSE.md) |
| [fmt](https://github.com/fmtlib/fmt) | [MIT](../app_root_ex/lib/LICENSES/fmt.LICENSE) |
| [spdlog](https://github.com/gabime/spdlog) | [MIT](../app_root_ex/lib/LICENSES/spdlog.LICENSE) |
| [unarr](https://github.com/selmf/unarr) | [LGPL-3.0](../app_root_ex/lib/LICENSES/unarr.LICENSE) |
| [zlib](https://github.com/madler/zlib) | [zlib](../app_root_ex/lib/LICENSES/zlib.LICENSE) |
| [chdman-simd](https://github.com/grouik1er-coder/chdman-simd) | [BSD-2-Clause](../app_root_ex/lib/LICENSES/chdman-simd.LICENSE) |
| [MAME](https://github.com/mamedev/mame) | [GPL-2.0+](../app_root_ex/lib/LICENSES/mame.LICENSE) |
| [libchdr](https://github.com/rtissera/libchdr) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/libchdr.LICENSE) |
| [Dolphin](https://github.com/dolphin-emu/dolphin) | [GPL-2.0+](../app_root_ex/lib/LICENSES/dolphin.LICENSE) |
| [Cemu](https://github.com/cemu-project/Cemu) | [MPL-2.0](../app_root_ex/lib/LICENSES/cemu.LICENSE) |
| [ZArchive](https://github.com/Exzap/ZArchive) | [MIT-0](../app_root_ex/lib/LICENSES/ZArchive.LICENSE) |
| [zstd](https://github.com/facebook/zstd) | [BSD-3-Clause](../app_root_ex/lib/LICENSES/zstd.LICENSE) |
| [extract-xiso](https://github.com/XboxDev/extract-xiso) | [BSD](../app_root_ex/lib/LICENSES/extract-xiso.LICENSE) |
| [7-Zip-zstd](https://github.com/mcmilk/7-Zip-zstd) | [LGPL-2.1+](../app_root_ex/lib/LICENSES/7-Zip-zstd.LICENSE) |
| [zlib-ng](https://github.com/zlib-ng/zlib-ng) | [zlib](../app_root_ex/lib/LICENSES/zlib-ng.LICENSE) |

## Licence

[AGPL-3.0](../LICENSE), like romexplorer.
