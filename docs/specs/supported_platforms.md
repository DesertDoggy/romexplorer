# RomExplorer metadata and detection specs

## Supported platforms

The project currently targets the ROM families that are common in Nintendo, Sony, Sega, NEC, Bandai, SNK, Microsoft, and MSX ecosystems.

- Nintendo: NES/Famicom, Super Famicom, Nintendo 64, Game Boy, Game Boy Color, Game Boy Advance, GameCube, Wii, Wii U, Nintendo DS, Nintendo 3DS, Virtual Boy, Nintendo Switch (.nsp, .nsz, .xci, .xcz, read without keys: the container, a ticket's title id, and each NCA in an .nsp hashed for No-Intro's digital DATs)
- Sony: PlayStation, PlayStation 2, PlayStation 3, PlayStation Portable, PlayStation Vita (.pkg, .vpk, .psv, game-card .img, app folders); PS3/PSP/Vita downloads (.pkg, game folders) say whether they are a game, an update or an add-on
- Sega: Master System, Game Gear, Genesis/Mega Drive, SG1000, 32X, Saturn, Sega CD, Dreamcast
- NEC: TurboGrafx-16, TurboGrafx-CD, SuperGrafx, PC Engine variants
- MSX, MSX2 with platform-specific extensions only (.mx1 and .mx2); generic .dsk/.rom/.cas/.ri files are not treated as supported
- PC-88 with platform-specific extensions only (.d88 and .88d); generic .iso/.bin/.wav/.flac are not treated as supported
- PC-98 is not supported yet
- PC-FX with platform-specific extension only (.pcfx); generic .iso/.bin/.wav/.flac are not treated as supported
- FM Towns and Sharp X68000 remain reserved for platform-specific extension detection only and are not supported via generic ISO/WAV/FLAC images
- Bandai: WonderSwan, WonderSwan Color
- SNK: Neo Geo CD, Neo Geo Pocket, Neo Geo Pocket Color
- Microsoft: Xbox, Xbox 360


## Future Plans
### Planned
- Neo Geo AES
- MSX Turbo, tape-based MSX variants, and other hidden-file or multi-disc layouts
- Arcade / other: Neo Geo MVS, arcade dumps
- Pokemon mini remains planned-only and is not treated as an active supported platform in the quick path
- FM Towns, Sharp X68000, and PC-FX are not supported via generic .iso/.bin/.wav/.flac inputs; only their platform-specific extensions are accepted
- PC-98 remains unsupported for now

### Modern Consoles
- PlayStation 4/5, Xbox One, Xbox Series, Nintendo Switch 2

### Not yet supported
- Pokemon mini
- Generic audio/image dumps such as .flac, .wav, or raw .iso for the niche legacy families above

