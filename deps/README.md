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
| cemu | cemu-project/Cemu | none; `wiiu_wua` compiles the files it needs |
| extract-xiso | XboxDev/extract-xiso | none; `exiso.c` compiles `extract-xiso.c` as it is |
| beetle-psx-libretro | libretro/beetle-psx-libretro | none; `psxpbp.c` compiles its PBP reader |
| playmatch | RetroRealm/playmatch | `patches/01_lan_server_limits.patch` (reference only; not built) |

Patches are applied to the submodule checkout by the build script, idempotently, so a
patched submodule shows as modified in `git status` after a build. That is expected.

The previous forks under `submodules/` are kept for now as a backup and are no longer read
by any script.
