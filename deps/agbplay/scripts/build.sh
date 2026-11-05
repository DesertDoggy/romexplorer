#!/usr/bin/env bash
set -euo pipefail

# Builds ragb.dll / libragb.so / libragb.dylib: GBA music from the ROM through agbplay
# (external/agbplay, LGPL-3.0), behind the ragb_ functions in deps/agbplay/shim/ragb.cpp. Only
# agbplay's playback core is compiled (deps/agbplay/CMakeLists.txt), so the library needs no
# libzip, zlib, libsndfile, JSON or Boost; fmt comes header-only from external/fmt.
#
# Usage:
#   deps/agbplay/scripts/build.sh                    # auto-detect platform/arch
#   deps/agbplay/scripts/build.sh <platform> <arch>  # linux|mac|windows  x64|arm64
#
# Output: deps/agbplay/release/<platform>/<arch>/<version>/dynamic/<library>, the layout
# scripts/move_libs_to_app_root.sh and src/rom/native_lib.rs expect.
#
# agbplay is C++23: GCC 13 or newer (MSYS2's MinGW GCC on Windows). On Windows the MinGW runtime
# is linked in statically, so ragb.dll imports only system DLLs.

if [[ $# -ne 0 && $# -ne 2 ]]; then
  echo "[ERROR] Usage: $0  OR  $0 <platform> <arch>" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
user_dir="$(cd "$script_dir/.." && pwd)"
main_root="$(cd "$user_dir/../.." && pwd)"
agb_root="$(cd "${AGBPLAY_SOURCE_DIR:-$main_root/external/agbplay}" && pwd)"
fmt_root="$(cd "${FMT_SOURCE_DIR:-$main_root/external/fmt}" && pwd)"
log_dir="$user_dir/logs"
mkdir -p "$log_dir"
log_file="$log_dir/build-agbplay-$(date +%Y%m%d-%H%M%S).log"

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
  windows) lib_name="ragb.dll"; system_name="Windows"; built_glob="*ragb.dll" ;;
  mac) lib_name="libragb.dylib"; system_name="Darwin"; built_glob="libragb*.dylib" ;;
  linux) lib_name="libragb.so"; system_name="Linux"; built_glob="libragb.so*" ;;
  *) echo "[ERROR] Unsupported platform: $platform (linux | mac | windows)" >&2; exit 2 ;;
esac

command -v cmake >/dev/null 2>&1 || { echo "[ERROR] cmake was not found." >&2; exit 2; }

# agbplay has no release tags; the commit date names the build.
output_version="$(git -C "$agb_root" log -1 --format=%cd --date=format:%Y%m%d 2>/dev/null || date +%Y%m%d)"
out_dir="$user_dir/release/$platform/$arch/$output_version"
build_dir="$(mktemp -d "${TMPDIR:-/tmp}/ragb_build.XXXXXX")"
trap 'rm -rf "$build_dir"' EXIT

cmake_args=(-S "$user_dir" -B "$build_dir" -DCMAKE_BUILD_TYPE=Release "-DCMAKE_SYSTEM_PROCESSOR=$processor"
  "-DAGBPLAY_ROOT=$agb_root" "-DFMT_ROOT=$fmt_root")
[[ -n "${CC:-}" ]] && cmake_args+=("-DCMAKE_C_COMPILER=$CC")
[[ -n "${CXX:-}" ]] && cmake_args+=("-DCMAKE_CXX_COMPILER=$CXX")
if [[ "$platform" != "$host_platform" || "$arch" != "$host_arch" ]]; then
  cmake_args+=("-DCMAKE_SYSTEM_NAME=$system_name")
  [[ "$platform" == "mac" ]] && cmake_args+=("-DCMAKE_OSX_ARCHITECTURES=$([[ "$arch" == "arm64" ]] && echo arm64 || echo x86_64)")
fi
if [[ "$host_platform" == "windows" ]]; then
  if command -v ninja >/dev/null 2>&1; then cmake_args+=(-G Ninja); else cmake_args+=(-G "MinGW Makefiles"); fi
fi

jobs="${JOBS:-$( (command -v nproc >/dev/null 2>&1 && nproc) || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"

{
  echo "[INFO] ragb $platform/$arch -> $out_dir"
  echo "[INFO] cmake ${cmake_args[*]}"
  cmake "${cmake_args[@]}"
  cmake --build "$build_dir" --target ragb -j "$jobs"
} 2>&1 | tee -a "$log_file"

built="$(find "$build_dir" -name "$built_glob" -type f ! -name '*.a' | head -1)"
if [[ -z "$built" ]]; then
  echo "[ERROR] the ragb library was not produced; see $log_file" >&2
  exit 1
fi

mkdir -p "$out_dir/dynamic"
cp "$built" "$out_dir/dynamic/$lib_name"
{
  echo "platform=$platform"
  echo "arch=$arch"
  echo "version=$output_version"
  echo "commit=$(git -C "$agb_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "fmt_commit=$(git -C "$fmt_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "compiler=${CXX:-default}"
  echo "built=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$out_dir/build-info.txt"
echo "[INFO] built $out_dir/dynamic/$lib_name ($(wc -c < "$out_dir/dynamic/$lib_name") bytes)"
