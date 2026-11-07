#!/usr/bin/env bash
set -euo pipefail

# Builds rvgmstream.dll / librvgmstream.so / librvgmstream.dylib: vgmstream's game audio decoder,
# with a static audio-only FFmpeg from deps/ffmpeg when one is built, behind the flat C API in
# deps/vgmstream/rvgm.h (see that header for why there is a shim at all). The build is
# deps/vgmstream/CMakeLists.txt, which includes upstream's source lists and edits nothing upstream.
#
# Usage:
#   deps/vgmstream/scripts/build.sh                    # auto-detect platform/arch
#   deps/vgmstream/scripts/build.sh <platform> <arch>  # linux|mac|windows  x64|arm64, or android arm64
#
# Output: deps/vgmstream/release/<platform>/<arch>/<version>/dynamic/<library>, the layout
# scripts/move_libs_to_app_root.sh and src/rom/native_lib.rs expect.
#
# The compiler is CC from the environment when set (scripts/build_native_deps.sh exports the
# cross compiler for windows/arm64 and linux/arm64 that way), else the host's default. On
# Windows run it from an MSYS2 MINGW64 shell, like the other native builds.

if [[ $# -ne 0 && $# -ne 2 ]]; then
  echo "[ERROR] Usage: $0  OR  $0 <platform> <arch>" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
user_dir="$(cd "$script_dir/.." && pwd)"
main_root="$(cd "$user_dir/../.." && pwd)"
# The upstream vgmstream tree; this script, the shim and the build output live in user_dir.
repo_root="$(cd "${VGM_SOURCE_DIR:-$main_root/external/vgmstream}" && pwd)"
log_dir="$user_dir/logs"
mkdir -p "$log_dir"
log_file="$log_dir/build-rvgmstream-$(date +%Y%m%d-%H%M%S).log"

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
  windows) lib_name="rvgmstream.dll"; system_name="Windows" ;;
  mac) lib_name="librvgmstream.dylib"; system_name="Darwin" ;;
  linux) lib_name="librvgmstream.so"; system_name="Linux" ;;
  android) lib_name="librvgmstream.so"; system_name="Android" ;;
  *) echo "[ERROR] Unsupported platform: $platform (linux | mac | windows | android)" >&2; exit 2 ;;
esac

command -v cmake >/dev/null 2>&1 || { echo "[ERROR] cmake was not found." >&2; exit 2; }

# A date, not `git describe`: upstream carries no tags here, and a bare commit hash is not a
# name the deploy script recognises as a version directory.
output_version="$(git -C "$repo_root" log -1 --format=%cd --date=format:%Y%m%d 2>/dev/null || date +%Y%m%d)"
out_dir="$user_dir/release/$platform/$arch/$output_version"
build_dir="$(mktemp -d "${TMPDIR:-/tmp}/rvgm_build.XXXXXX")"
trap 'rm -rf "$build_dir"' EXIT

cmake_args=(-S "$user_dir" -B "$build_dir" "-DVGM_ROOT=$repo_root" -DCMAKE_BUILD_TYPE=Release "-DCMAKE_SYSTEM_PROCESSOR=$processor")
# FFmpeg (deps/ffmpeg/scripts/build.sh), linked in statically when its release tree for this
# platform/arch exists: the newest version directory wins. Without it rvgmstream still builds,
# and WMA, XMA and ATRAC3 streams stay unplayable. VGM_NO_FFMPEG=1 skips it on purpose.
ffmpeg_root=""
if [[ "${VGM_NO_FFMPEG:-0}" != "1" ]]; then
  ffmpeg_root="$(ls -d "$main_root/deps/ffmpeg/release/$platform/$arch"/*/static 2>/dev/null | sort -V | tail -1 || true)"
fi
if [[ -n "$ffmpeg_root" ]]; then
  cmake_args+=("-DFFMPEG_ROOT=$ffmpeg_root")
else
  echo "[WARN] no FFmpeg build for $platform/$arch under deps/ffmpeg/release: WMA, XMA and ATRAC3 will not decode"
fi
if [[ -n "${CC:-}" ]]; then
  cmake_args+=("-DCMAKE_C_COMPILER=$CC")
fi
# Android: the NDK's own CMake toolchain file, which picks its compilers and sysroot. API level
# 28 (Android 9) by default: libvgm's text conversion uses iconv, which Android's libc has
# only from 28 on. ANDROID_API overrides it.
if [[ "$platform" == "android" ]]; then
  [[ "$arch" == "arm64" ]] || { echo "[ERROR] android is arm64 only" >&2; exit 2; }
  [[ -n "${ANDROID_NDK_HOME:-}" ]] || { echo "[ERROR] ANDROID_NDK_HOME is not set" >&2; exit 2; }
  ndk_toolchain="$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake"
  [[ -f "$ndk_toolchain" ]] || { echo "[ERROR] no NDK toolchain file at $ndk_toolchain" >&2; exit 2; }
  # The toolchain file brings its own compilers and processor: none from the environment.
  unset CC CXX
  kept=()
  for a in "${cmake_args[@]}"; do
    case "$a" in -DCMAKE_SYSTEM_PROCESSOR=*|-DCMAKE_C_COMPILER=*|-DCMAKE_CXX_COMPILER=*) ;; *) kept+=("$a") ;; esac
  done
  cmake_args=("${kept[@]}")
  cmake_args+=("-DCMAKE_TOOLCHAIN_FILE=$ndk_toolchain" -DANDROID_ABI=arm64-v8a "-DANDROID_PLATFORM=android-${ANDROID_API:-28}" -DANDROID_STL=c++_static)
fi
# Cross builds: tell cmake the target system so it does not try to run what it compiles.
if [[ "$platform" != "android" ]] && [[ "$platform" != "$host_platform" || "$arch" != "$host_arch" ]]; then
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
  echo "[INFO] rvgmstream $platform/$arch -> $out_dir"
  echo "[INFO] cmake ${cmake_args[*]}"
  cmake "${cmake_args[@]}"
  cmake --build "$build_dir" --target rvgmstream -j "$jobs"
} 2>&1 | tee "$log_file"

built="$(find "$build_dir" -name "$lib_name" -type f | head -1)"
if [[ -z "$built" ]]; then
  echo "[ERROR] $lib_name was not produced; see $log_file" >&2
  exit 1
fi

mkdir -p "$out_dir/dynamic" "$out_dir/include"
cp "$built" "$out_dir/dynamic/$lib_name"
cp "$user_dir/rvgm.h" "$out_dir/include/rvgm.h"
{
  echo "platform=$platform"
  echo "arch=$arch"
  echo "commit=$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "compiler=${CC:-default}"
  echo "ffmpeg=${ffmpeg_root:-none}"
  echo "built=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$out_dir/build-info.txt"
echo "[INFO] built $out_dir/dynamic/$lib_name ($(wc -c < "$out_dir/dynamic/$lib_name") bytes)"
