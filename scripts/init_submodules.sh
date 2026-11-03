#!/usr/bin/env bash
# Checks out the external/ submodules, plus only the nested submodules our builds use.
#
# Don't clone with --recursive: some upstreams carry nested submodules we never build and
# that don't even resolve (beetle-psx-libretro's deps/lightning/gnulib has no url), and
# mariadb-server's rocksdb/columnstore/... are large and switched off by our build.
#
# Usage, from anywhere inside the checkout:  scripts/init_submodules.sh
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# Every external/<name>, at the commit this repo pins, without their own submodules.
git submodule update --init

# dolphin: Externals/ (some two levels deep). RALibretro: miniz, rcheevos and friends.
for d in external/dolphin external/RALibretro; do
  git -C "$d" submodule update --init --recursive
done

# mariadb-server: libmariadb only (TLS is our own OpenSSL, so no bundled wolfssl). Stop its
# CMake from fetching the rest at configure time.
git -C external/mariadb-server submodule update --init libmariadb
git -C external/mariadb-server config cmake.update-submodules no

git submodule status
