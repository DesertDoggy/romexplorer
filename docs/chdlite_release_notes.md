# chdlite 0.3.4

chdlite is released separately from romexplorer; these notes cover chdlite only.

## Changes

- **RetroAchievements.** The list ships as assets/radb.bin and can be replaced with a newer one.
- **Progress.** Shows each file's step and size, and every file running at once.
- Searching folders, checking formats and planning outputs show their progress and run faster.

# chdlite 0.3.3

## Changes

- **3DS.** .cci, .3ds, .cxi, .zcci and .zcxi output.
- **NES, N64 and SNES dumps.** Output as NES headered (.nes), headerless (.nes) or .unh,
  N64 .z64, .v64 or .n64, and SNES .sfc or .smc.
- NES header conversion works only for supported games, as it matches hashes against a
  database.
- **A Mac app and a Linux AppImage were added.**
- **RetroAchievements.** Hash supports RA hashes.

# chdlite 0.3.2

Android only.

## Bug fixes

- The Android app could not be allowed file access.

# chdlite 0.3.1

## Bug fixes

- On Windows arm64 and Linux arm64, Wii U, Xbox and Xbox 360 images could not be converted and
  PBP files could not be read.

# chdlite 0.3.0

## Changes

- **Now based on Rust.**
- **RVZ support.**
- **Wii U and Xbox.** WUA, XISO and ZAR output.
- **Lossless inputs only.**
- **The output format is picked automatically.** CHD and RVZ are output as ISO, and an ISO
  becomes a CHD, RVZ, XISO or ZAR to suit its platform.
- **One window, four modes.** The UI is now Lite (fast), Comp (smallest), Read (reads
  metadata) and Hash, switched with a toggle, with drag and drop.
- **Android support.**
- Windows and Linux ARM64 support.

## Bug fixes

- A conversion cut off by Ctrl-C, a crash or closing the window left a broken file that the
  next run took as done.
- A PS2 or PSP ISO of a certain size was taken for an audio CD.
- Cancel did not work while short jobs kept running.
- A settings file that could not be read was overwritten with the defaults. It is now kept as
  `chdlite.toml.bak`.
- `-hs` set the same hunk size for CDs and DVDs.

## Not in this release

- GCZ and WIA output.
- The chdman-compatible command names (`createcd`, `extractcd`, ...) and hard disk / raw CHDs.
- A single-`.bin` extract option: a CD always extracts to one file per track.
