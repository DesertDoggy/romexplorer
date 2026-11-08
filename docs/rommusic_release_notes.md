# rommusic 0.1.0

rommusic is released separately from romexplorer; these notes cover rommusic only.

## Changes

- **First release.** Drop a ROM, disc image, dumped folder or set of original-format music files on rommusic and
  it writes the game's background music as FLAC, in a folder beside it, with an index.txt.
- **A window, and an Android app.** `rommusic-gui` (Windows, Linux, macOS) and the Android
  app take files or a folder, FLAC or the original audio, and an output folder, with the
  other options behind a button.
- **Options on the command line:** all sounds instead of background music only, a track
  limit, an output folder, CD, DVD or hi-res rates, and the music in its original format
  (stream files, original-format files, an .spc for SNES).
- **Cartridges** are matched to your own original-format music files by their content (`--originals`). NES and
  Famicom (expansion sound chips included), Disk System, Game Boy, Game Boy Color, Game Boy Advance, Master
  System (FM included), Game Gear, Mega Drive, PC Engine, MSX, WonderSwan, Virtual Boy and Neo Geo Pocket music plays straight from the ROM, and SNES games give their title music without an original-format file.
- **Original-format tracks with no length** (or a guessed one) end where the music does: no silence at
  the end, and a repeating track plays its loop twice and fades.
- Original-format sets whose playlists count tracks from 0 play the right track for each name.
- **Wii and Wii U:** Wii discs, and Wii U .wua files, or .wud/.wux and downloaded titles
  when their key is beside them.
- The supported systems and formats are listed in README.md.
