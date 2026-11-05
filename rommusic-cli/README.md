# rommusic

Drop a ROM, disc image or music rip set on `rommusic` and it writes the game's music as FLAC.
It uses the same readers and players the romexplorer app plays ROM music with, and is
released on its own (version 0.1.0).

[日本語](README.ja.md)

## Using it

**Drag and drop.** Drop one or more files or folders on `rommusic.exe` (or `rommusic` on
Linux and macOS). For each one it makes a folder named `<name> (music)` next to it, holding:

- `01 - <track name>.flac`, `02 - ...`: the background music, at its original sample rate and
  16-bit. A looping track plays its loop twice and fades out, as the game plays it.
- `index.txt`: each track's number, kind, length, whether it loops, and what it was decoded
  from.

A file already there from an earlier run is replaced.

Dropping always gives FLAC with the defaults below. Everything else needs the command line.

**Command line.**

```
rommusic [options] <ROM, disc image, folder or rip set>...
```

Examples:

```
rommusic "Hoge Quest (Japan).chd"
rommusic --all --max 20 "Hoge Quest (Japan).chd"
rommusic --rips D:\rips --out D:\music "Hoge Quest (Japan).nes"
rommusic --hires "Hoge Quest (Japan).cue"
rommusic --format both "Hoge Quest (Japan).sfc"
```

## Options

| Option | What it does |
|---|---|
| `--all` | Also jingles, sound effects and the sound of movies. By default only background music is written. |
| `--max N` | At most N tracks from each input. |
| `--out DIR` | Put the `<name> (music)` folders in `DIR` instead of beside each input. |
| `--rate native` | The default: each track at the rate it is decoded at (see the table below). |
| `--cd` / `--rate cd` | 44.1 kHz, 16-bit. |
| `--dvd` / `--rate dvd` | 48 kHz, 24-bit. |
| `--hires` / `--rate hires` | 96 kHz, 24-bit. |
| `--rate <Hz>` | Any rate from 8000 to 192000. |
| `--bits 16\|24` | Bits per sample (16 by default, 24 with `--dvd` and `--hires`). |
| `--format flac` | The default: FLAC only. |
| `--format original` | The music as the game stores it, in an `original` folder, and no FLAC. |
| `--format both` | FLAC and the original files. |
| `--rips DIR` | A folder of music rip sets (NSF, SPC, GBS, HES, KSS, SGC; loose, in folders, or zipped) to match cartridges against by their content. File names do not matter. Can be given more than once. |
| `--seconds N` | SNES: how long the game runs before its music is taken (default 20). |
| `--start` | SNES: press Start now and then while the game runs, for games whose music starts after the title screen. |
| `--list` | Only list the tracks; write nothing. A looping track shows one pass and "loop"; its file plays the loop twice and fades. |
| `--no-pause` | Don't wait for Enter at the end. It only waits in the window that opens for a dropped file. |
| `-h`, `--help` / `-V`, `--version` | Help and version. |

A rate other than `native` is resampled. Resampling does not add detail; it is for players
that want every file at the same rate, or that handle unusual rates such as 37.8 kHz poorly.
A resampled file says so in its comment tag.

What `--format original` writes:

- Disc and folder games: the stream files as they are stored on the disc (ADX, XA, AT3,
  BCSTM...). When a stream is inside an archive on the disc, the whole archive is copied.
- Rip sets: the set's files (the NSF, SPC, GBS... and its playlist; a zipped set is copied
  as it is).
- SNES cartridges: an `.spc` of the sound chip after the game has run.
- CD audio and music sequenced in a cartridge (Game Boy Advance) have no original file. Use
  FLAC for these.

Original files hold the game's own code and data. Keep them with your ROMs and don't share
them.

## Supported formats

**Disc images and folders.** `.chd`, `.cue`/`.bin`, `.gdi`, `.iso`, `.img`, `.gcm`,
`.rvz`, `.wia`, `.gcz`, `.wbfs`, `.cso`, `.zso`, XISO, and dumped game folders.
Wii U: `.wua`, and `.wud`/`.wux` or a downloaded title folder when its key is beside it (a
`.key` file, or `keys.txt` in the same folder). No keys come with rommusic.

| Source | Systems | Rate (native) |
|---|---|---|
| CD audio tracks | PlayStation, Saturn, Sega CD, PC Engine CD, Neo Geo CD, Dreamcast, and others | 44.1 kHz |
| Music files on the disc, decoded by vgmstream, including ATRAC3, WMA and XMA | PlayStation, PlayStation 2, PSP, Saturn, Dreamcast, GameCube, Wii, Wii U, Xbox, Xbox 360, and others | as stored |
| Music inside common game archives on the disc (CRI CPK, AFS, GameCube .arc/.szs) | as above | as stored |

**Cartridges.**

| System | How | Rate (native) |
|---|---|---|
| Nintendo DS | Music streams in the sound archive (SDAT). Sequenced music is not played yet. | as stored |
| Nintendo 3DS | Music files in images stored decrypted. Encrypted images are not read. | as stored |
| Game Boy Advance | Straight from the ROM, for games using Nintendo's standard sound engine (about half of all games). | 48 kHz |
| SNES / Super Famicom | The game runs for a few seconds without a screen, and its sound chip is then played on. This gives one track, usually the title music. SA-1, Super FX and S-DD1 games are not supported. With `--rips`, a matching SPC set gives every song instead. | 32 kHz |
| NES, Famicom, FDS | Matching NSF set from `--rips`. | 44.1 kHz |
| Game Boy, Game Boy Color | Matching GBS set from `--rips`. | 44.1 kHz |
| PC Engine, SuperGrafx | Matching HES set from `--rips`. | 44.1 kHz |
| Master System, Game Gear, SG-1000, MSX, ColecoVision | Matching SGC or KSS set from `--rips`. | 44.1 kHz |

**Rip sets.** A rip file or zip dropped on its own is played as it is: NSF, NSFE, SPC, GBS,
HES, KSS, SGC, VGM, VGZ, GYM, AY, SAP. Track names and lengths come from the set's playlist
(`.m3u`) when it has one.

**Zip and 7z.** A zipped cartridge is read for Game Boy Advance and SNES and for matching rip
sets. A zipped disc image is not read; extract it first.

### Not supported yet

- Nintendo 64, WonderSwan, Neo Geo Pocket, Virtual Boy.
- Mega Drive / Genesis and 32X straight from the ROM. A VGM set is played when dropped on its
  own, but it can't be matched to a ROM.
- Game Boy Advance games that use other sound engines.
- Sequenced music on DS and 3DS.
- PS3, Vita and downloaded packages.
- Wii U disc images without their key.
- Encrypted 3DS images.

## Files beside it

`rommusic` loads its decoders from `lib/<os>/<arch>/` next to it: `rommusic` (the chip
players), `rvgmstream` (streams; FFmpeg is inside it), and the disc and archive readers
(`chdman`, `dolphinrvz`, `cemuwua`, `exiso`, 7-Zip). Keep the folder together. Licences are
in `lib/LICENSES/`.

## Licence

AGPL-3.0, like romexplorer. The bundled libraries keep their own licences (LGPL and GPL-2.0+
for the players, see `lib/LICENSES/`).
