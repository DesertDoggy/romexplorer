# rommusic 0.1.0

rommusic is released separately from romexplorer; these notes cover rommusic only.

## Changes

- **First release.** Drop a ROM, disc image, dumped folder or music rip set on rommusic and
  it writes the game's background music as FLAC, in a folder beside it, with an index.txt.
- **Options on the command line:** all sounds instead of background music only, a track
  limit, an output folder, CD, DVD or hi-res rates, and the music in its original format
  (stream files, rip sets, an .spc for SNES).
- **Cartridges** are matched to your own rip sets by their content (`--rips`). Game Boy
  Advance music plays straight from the ROM, and SNES games give their title music without
  a rip.
- **Rip tracks with no length** (or a guessed one) end where the music does: no silence at
  the end, and a repeating track plays its loop twice and fades.
- Rip sets whose playlists count tracks from 0 play the right track for each name.
- **Wii and Wii U:** Wii discs, and Wii U .wua files, or .wud/.wux and downloaded titles
  when their key is beside them.
- The supported systems and formats are listed in README.md.
