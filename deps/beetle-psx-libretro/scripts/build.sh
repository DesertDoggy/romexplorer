#!/usr/bin/env bash
set -euo pipefail

# Builds psxpbp.dll / libpsxpbp.so / libpsxpbp.dylib: PlayStation discs read out of a PSP
# EBOOT.PBP through Beetle PSX's own PBP reader (see deps/beetle-psx-libretro/psxpbp.h). The
# build is deps/beetle-psx-libretro/CMakeLists.txt, which compiles the Beetle and
# libretro-common sources the reader needs as they are.
#
# Usage:
#   deps/beetle-psx-libretro/scripts/build.sh                    # auto-detect platform/arch
#   deps/beetle-psx-libretro/scripts/build.sh <platform> <arch>  # linux|mac|windows  x64|arm64
#
# Source: external/beetle-psx-libretro (override with BEETLE_SOURCE_DIR).
#
# Output: deps/beetle-psx-libretro/release/<platform>/<arch>/<version>/dynamic/<library>, the layout
# scripts/move_libs_to_app_root.sh and src/rom/native_lib.rs expect. <version> is the date of
# the beetle-psx-libretro commit built.
#
# The compilers are CC from the environment when set, else the host's default. On Windows
# run it from an MSYS2 MINGW64 shell, like the other native builds.

if [[ $# -ne 0 && $# -ne 2 ]]; then
  echo "[ERROR] Usage: $0  OR  $0 <platform> <arch>" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
user_dir="$(cd "$script_dir/.." && pwd)"
main_root="$(cd "$user_dir/../.." && pwd)"
src_root="$(cd "${BEETLE_SOURCE_DIR:-$main_root/external/beetle-psx-libretro}" && pwd)"
log_dir="$user_dir/logs"
mkdir -p "$log_dir"
log_file="$log_dir/build-psxpbp-$(date +%Y%m%d-%H%M%S).log"

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
  windows) lib_name="psxpbp.dll"; system_name="Windows" ;;
  mac) lib_name="libpsxpbp.dylib"; system_name="Darwin" ;;
  linux) lib_name="libpsxpbp.so"; system_name="Linux" ;;
  *) echo "[ERROR] Unsupported platform: $platform (linux | mac | windows)" >&2; exit 2 ;;
esac

command -v cmake >/dev/null 2>&1 || { echo "[ERROR] cmake was not found." >&2; exit 2; }

output_version="$(git -C "$src_root" log -1 --format=%cd --date=format:%Y%m%d 2>/dev/null || date +%Y%m%d)"
out_dir="$user_dir/release/$platform/$arch/$output_version"
build_dir="$(mktemp -d "${TMPDIR:-/tmp}/psxpbp_build.XXXXXX")"
trap 'rm -rf "$build_dir"' EXIT

cmake_args=(-S "$user_dir" -B "$build_dir" -DCMAKE_BUILD_TYPE=Release
  "-DBEETLE_ROOT=$src_root")
[[ -n "${CC:-}" ]] && cmake_args+=("-DCMAKE_C_COMPILER=$CC")
# Cross builds: tell cmake the target system so it does not try to run what it compiles.
if [[ "$platform" != "$host_platform" || "$arch" != "$host_arch" ]]; then
  cmake_args+=("-DCMAKE_SYSTEM_NAME=$system_name" "-DCMAKE_SYSTEM_PROCESSOR=$processor")
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
  echo "[INFO] psxpbp $platform/$arch -> $out_dir"
  echo "[INFO] cmake ${cmake_args[*]}"
  cmake "${cmake_args[@]}"
  cmake --build "$build_dir" --target psxpbp -j "$jobs"
} 2>&1 | tee "$log_file"

built="$(find "$build_dir" -name "$lib_name" -type f | head -1)"
if [[ -z "$built" ]]; then
  echo "[ERROR] $lib_name was not produced; see $log_file" >&2
  exit 1
fi

mkdir -p "$out_dir/dynamic" "$out_dir/include"
cp "$built" "$out_dir/dynamic/$lib_name"
cp "$user_dir/psxpbp.h" "$out_dir/include/psxpbp.h"
{
  echo "platform=$platform"
  echo "arch=$arch"
  echo "beetle_commit=$(git -C "$src_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "built=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$out_dir/build-info.txt"
echo "[INFO] built $out_dir/dynamic/$lib_name ($(wc -c < "$out_dir/dynamic/$lib_name") bytes)"
