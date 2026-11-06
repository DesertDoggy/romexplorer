# Native dependencies

Each directory here belongs to one native library the app loads: its build script, the shim
we wrap it in, and any patches to its sources. The sources themselves are upstream
submodules under `external/<name>/`, pinned to a specific upstream commit, so updating a
library is a submodule bump plus (where there is one) refreshing its patch.

Build output stays here too, in `<name>/release/`, `<name>/_build/` and `<name>/logs/`
(git-ignored); `scripts/move_libs_to_app_root.sh` deploys from `<name>/release/`.
`scripts/build_native_deps.sh` builds them all in order.

| dependency | upstream (external/) | our changes to its sources |
|---|---|---|
| zlib-ng | zlib-ng/zlib-ng | none |
| openssl | openssl/openssl | none |
| mariadb-connector-c | mariadb-corporation/mariadb-connector-c | none |
| mariadb-server | MariaDB/server | `patches/01_client_dynamic_link.patch` |
| 7-Zip-zstd | mcmilk/7-Zip-zstd | `patches/01_streaming_extract.patch` (the streaming extract callbacks) |
| chdman-simd | grouik1er-coder/chdman-simd | its own `patches/` plus ours in `patches/` here; sources come from external/mame |
| dolphin | dolphin-emu/dolphin | none |
| RALibretro | RetroAchievements/RALibretro | `patches/01_hash3ds_stdlib.patch` |
| libxaac | ittiam-systems/libxaac | none |
| ffmpeg | FFmpeg/FFmpeg (tag n9.0.2) | none; built static, LGPL, audio decoders only, and linked into rvgmstream (no DLL of its own) |
| vgmstream | vgmstream/vgmstream | none; `rvgm.c` wraps its API, `CMakeLists.txt` compiles its sources and links ffmpeg's static build when there is one |
| rommusic | libgme/game-music-emu (tag 0.6.5), kode54/Game_Music_Emu (external/gme-kode54), ipatix/agbplay (LGPL-3.0), kode54/SSEQPlayer (external/SSEQPlayer, WTFPL), fmt | One library (`CMakeLists.txt`): upstream gme as it is (Nuked YM2612, no zlib), linked in whole; `spc/rspc.cpp` drives its Snes_Spc through the ports; `sgc/` builds only the fork's SGC player (YM2413 silent) behind `rsgc.cpp`, with `sgc/rename.h` (from `sgc/make_rename.sh`) prefixing the fork's names so they do not collide with upstream's, and `patches/01_sgc_rst_vectors.patch` (an SGC's RST vectors were read one entry late); `agbplay/` builds only agbplay's playback core behind `ragb.cpp`, `rom_buffer.cpp` replacing upstream's Rom.cpp (no file, zip or GSF loading) and `compat/` standing in for the one Boost function used; `sseq/rsseq.cpp` puts a C API over SSEQPlayer (DS sequenced music), with `patches/sseq/` bounds-checking its reading of the sound archive, building it as C++17, recording where each track jumps back (`rsseq_timing`), mixing 6 dB down and soft-limiting peaks |
| cemu | cemu-project/Cemu | none; `wiiu_wua` compiles the files it needs |
| extract-xiso | XboxDev/extract-xiso | none; `exiso.c` compiles `extract-xiso.c` as it is |
| beetle-psx-libretro | libretro/beetle-psx-libretro | none; `psxpbp.c` compiles its PBP reader |
| playmatch | RetroRealm/playmatch | `patches/01_lan_server_limits.patch` (reference only; not built) |

Patches are applied to the submodule checkout by the build script, idempotently, so a
patched submodule shows as modified in `git status` after a build. That is expected.

The previous forks under `submodules/` are kept for now as a backup and are no longer read
by any script.
