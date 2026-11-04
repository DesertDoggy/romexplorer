#!/usr/bin/env bash
set -euo pipefail

# Builds gme.dll / libgme.so / libgme.dylib: Game Music Emu (LGPL-2.1), the player for rip sets
# of older consoles (NSF/NSFE, SPC, GBS, VGM/VGZ, GYM, HES, KSS, AY, SAP). gme.h is already a
# flat C API, so there is no shim and upstream's own CMake build is used unchanged.
#
# Usage:
#   deps/gme/scripts/build.sh                    # auto-detect platform/arch
#   deps/gme/scripts/build.sh <platform> <arch>  # linux|mac|windows  x64|arm64
#
# Output: deps/gme/release/<platform>/<arch>/<version>/dynamic/<library>, the layout
# scripts/move_libs_to_app_root.sh and src/rom/native_lib.rs expect.
#
# Options fixed here, and why:
#   - Nuked YM2612 (LGPL), not MAME's (GPL), so the library stays LGPL.
#   - No zlib: .vgz is gunzipped on the Rust side through rom::zlib_ng, so the DLL has no
#     dependency of its own to deploy.
#   - On Windows the MinGW runtime (libstdc++, libgcc, winpthread) is linked in statically, so
#     gme.dll imports only system DLLs.
#
# The compiler is CXX/CC from the environment when set (scripts/build_native_deps.sh exports the
# cross compiler that way), else the host's default. On Windows run it from an MSYS2 MINGW64
# shell, like the other native builds.

if [[ $# -ne 0 && $# -ne 2 ]]; then
  echo "[ERROR] Usage: $0  OR  $0 <platform> <arch>" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
user_dir="$(cd "$script_dir/.." && pwd)"
main_root="$(cd "$user_dir/../.." && pwd)"
repo_root="$(cd "${GME_SOURCE_DIR:-$main_root/external/game-music-emu}" && pwd)"
log_dir="$user_dir/logs"
mkdir -p "$log_dir"
log_file="$log_dir/build-gme-$(date +%Y%m%d-%H%M%S).log"

os_name="$(uname -s)"
cpu_name="$(uname -m)"
case "$os_name" in
  Darwin) host_platform="mac" ;;
  Linux) host_platform="linux" ;;
  MINGW*|MSYS*|CYGWIN*) host_platform="windows" ;;
  *) echo "[ERROR] Unsupported OS: $os_name" >&2; exit 2 ;;
esac
case "$cpu_name" in
  arm64|aarch64) host_arch="arm64" ;;
  x86_64|amd64) host_arch="x64" ;;
  *) echo "[ERROR] Unsupported arch: $cpu_name" >&2; exit 2 ;;
esac

platform="${1:-$host_platform}"
arch="${2:-$host_arch}"
case "$arch" in
  x64) processor="x86_64" ;;
  arm64) processor="aarch64" ;;
  *) echo "[ERROR] Unsupported arch: $arch (x64 | arm64)" >&2; exit 2 ;;
esac
case "$platform" in
  windows) lib_name="gme.dll"; system_name="Windows"; built_glob="*gme*.dll" ;;
  mac) lib_name="libgme.dylib"; system_name="Darwin"; built_glob="libgme*.dylib" ;;
  linux) lib_name="libgme.so"; system_name="Linux"; built_glob="libgme.so*" ;;
  *) echo "[ERROR] Unsupported platform: $platform (linux | mac | windows)" >&2; exit 2 ;;
esac

command -v cmake >/dev/null 2>&1 || { echo "[ERROR] cmake was not found." >&2; exit 2; }

# The release tag (0.6.5 ...) when the checkout sits on one, else the commit date.
output_version="$(git -C "$repo_root" describe --tags --exact-match 2>/dev/null || git -C "$repo_root" log -1 --format=%cd --date=format:%Y%m%d 2>/dev/null || date +%Y%m%d)"
out_dir="$user_dir/release/$platform/$arch/$output_version"
build_dir="$(mktemp -d "${TMPDIR:-/tmp}/gme_build.XXXXXX")"
trap 'rm -rf "$build_dir"' EXIT

cmake_args=(-S "$repo_root" -B "$build_dir" -DCMAKE_BUILD_TYPE=Release "-DCMAKE_SYSTEM_PROCESSOR=$processor"
  -DGME_BUILD_SHARED=ON -DGME_BUILD_STATIC=OFF -DGME_BUILD_FRAMEWORK=OFF
  -DGME_BUILD_TESTING=OFF -DGME_BUILD_EXAMPLES=OFF -DBUILD_TESTING=OFF
  -DGME_ZLIB=OFF -DGME_YM2612_EMU=Nuked)
[[ -n "${CC:-}" ]] && cmake_args+=("-DCMAKE_C_COMPILER=$CC")
[[ -n "${CXX:-}" ]] && cmake_args+=("-DCMAKE_CXX_COMPILER=$CXX")
if [[ "$platform" == "windows" ]]; then
  # -static, not just -static-libstdc++: the winpthread that libstdc++ pulls in is resolved
  # after the objects, so anything less leaves gme.dll importing libwinpthread-1.dll.
  cmake_args+=("-DCMAKE_SHARED_LINKER_FLAGS=-static -static-libgcc -static-libstdc++")
fi
# Cross builds: tell cmake the target system so it does not try to run what it compiles.
if [[ "$platform" != "$host_platform" || "$arch" != "$host_arch" ]]; then
  cmake_args+=("-DCMAKE_SYSTEM_NAME=$system_name")
  [[ "$platform" == "mac" ]] && cmake_args+=("-DCMAKE_OSX_ARCHITECTURES=$([[ "$arch" == "arm64" ]] && echo arm64 || echo x86_64)")
fi
if [[ "$host_platform" == "windows" ]]; then
  # MSYS2's cmake defaults to Visual Studio when one is installed; this is a MinGW build.
  if command -v ninja >/dev/null 2>&1; then
    cmake_args+=(-G Ninja)
  else
    cmake_args+=(-G "MinGW Makefiles")
  fi
fi

jobs="${JOBS:-$( (command -v nproc >/dev/null 2>&1 && nproc) || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"

{
  echo "[INFO] gme $platform/$arch -> $out_dir"
  echo "[INFO] cmake ${cmake_args[*]}"
  cmake "${cmake_args[@]}"
  cmake --build "$build_dir" --target gme_shared -j "$jobs"
} 2>&1 | tee "$log_file"

# The real file, not a SONAME symlink (libgme.so.0 -> libgme.so.0.6.5). MinGW names it
# libgme.dll; the app looks for gme.dll, like every other library it loads.
built="$(find "$build_dir" -name "$built_glob" -type f ! -name '*.a' | head -1)"
if [[ -z "$built" ]]; then
  echo "[ERROR] the gme library was not produced; see $log_file" >&2
  exit 1
fi

mkdir -p "$out_dir/dynamic" "$out_dir/include"
cp "$built" "$out_dir/dynamic/$lib_name"
cp "$repo_root/gme/gme.h" "$out_dir/include/gme.h"
{
  echo "platform=$platform"
  echo "arch=$arch"
  echo "version=$output_version"
  echo "commit=$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "compiler=${CXX:-default}"
  echo "built=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$out_dir/build-info.txt"
echo "[INFO] built $out_dir/dynamic/$lib_name ($(wc -c < "$out_dir/dynamic/$lib_name") bytes)"
