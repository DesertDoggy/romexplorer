#!/usr/bin/env bash
set -euo pipefail

# Builds the whole Beetle PSX libretro core, software renderer only, for tools/psx_probe (an
# analysis tool that runs a disc headless to find where its music sits). Not shipped.
#
# Usage: deps/beetle-psx-libretro/scripts/build_core.sh
#
# Source: external/beetle-psx-libretro (override with BEETLE_SOURCE_DIR). Its Makefile builds
# in the source tree; the objects are cleaned away afterwards. JOBS sets the parallel jobs
# (default 4).
#
# Output: deps/beetle-psx-libretro/release/<platform>/<arch>/core/<library>.
# On Windows run it from an MSYS2 MINGW64 shell, like the other native builds.

script_dir="$(cd "$(dirname "$0")" && pwd)"
user_dir="$(cd "$script_dir/.." && pwd)"
main_root="$(cd "$user_dir/../.." && pwd)"
src_root="$(cd "${BEETLE_SOURCE_DIR:-$main_root/external/beetle-psx-libretro}" && pwd)"
jobs="${JOBS:-4}"

case "$(uname -s)" in
  Darwin) platform="mac"; make_platform="osx"; lib="mednafen_psx_libretro.dylib" ;;
  Linux) platform="linux"; make_platform="unix"; lib="mednafen_psx_libretro.so" ;;
  MINGW*|MSYS*|CYGWIN*) platform="windows"; make_platform="win"; lib="mednafen_psx_libretro.dll" ;;
  *) echo "[ERROR] Unsupported OS" >&2; exit 2 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) arch="arm64" ;;
  *) arch="x64" ;;
esac

out_dir="$user_dir/release/$platform/$arch/core"
mkdir -p "$out_dir"
make -C "$src_root" -j"$jobs" HAVE_HW=0 HAVE_CDROM=0 platform="$make_platform"
cp "$src_root/$lib" "$out_dir/"
make -C "$src_root" clean HAVE_HW=0 HAVE_CDROM=0 platform="$make_platform" >/dev/null
echo "[OK] $out_dir/$lib"
