# rommusic 0.2.1

rommusic is released separately from romexplorer; these notes cover rommusic only.

## Changes

- **Cartridges with a known music map are written faster.**

## Bug fixes

- SNES games with no music map wrote no music.
- The Android app kept the old music maps after an update.

# rommusic 0.2.0

rommusic is released separately from romexplorer; these notes cover rommusic only.

## Changes

- **More music straight from the ROM.** NES, Famicom Disk System, Game Boy, PC Engine and
  WonderSwan were added (supported games only).
- **A Mac app and a Linux AppImage were added.**
- **More options in the window.** The track limit, the bit depth, and a sample rate in Hz
  can be set.

## Bug fixes

- Writing the music files never finished on discs with a ";" inside a file name.
- The Android app could not be allowed file access.
- The Android app now has its own icon.
- The Android app did not carry the music maps.
- Two inputs of the same name written to one folder mixed their tracks.
- The Android app could not read ROMs inside zip and 7z.

# rommusic 0.1.0

## Changes

- **First release.** Drop a ROM, disc image or dumped folder on rommusic and it writes the game's background music as FLAC.
- **A window, and an Android app.** `rommusic-gui` (Windows, Linux, macOS) and the Android
  app take files or a folder, FLAC or the original format, and an output folder, with the
  other options behind a button.
- **Options on the command line:** all sounds instead of background music only, a track
  limit, an output folder, CD, DVD or hi-res rates, and the music in its original format
  (stream files, an .spc for SNES).
- **Cartridges:** NES and
  Famicom (expansion sound chips included), Disk System, Game Boy, Game Boy Color, Game Boy Advance, Master
  System (FM included), Game Gear, Mega Drive, PC Engine, MSX, WonderSwan, Virtual Boy and Neo Geo Pocket music plays straight from the ROM, and SNES games give their title music.
- **Wii and Wii U:** Wii discs, and Wii U .wua files, or .wud/.wux and downloaded titles
  when their key is beside them.
- The supported systems and formats are listed in README.md.
