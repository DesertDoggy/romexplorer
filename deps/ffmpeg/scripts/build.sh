#!/usr/bin/env bash
set -euo pipefail

# Builds a static, audio-only FFmpeg (libavformat, libavcodec, libswresample, libavutil) for
# deps/vgmstream to link into rvgmstream. Nothing here is deployed on its own: the libraries
# end up inside rvgmstream.dll / librvgmstream.so / librvgmstream.dylib.
#
# What it adds to rvgmstream: the game music codecs vgmstream hands to FFmpeg, chiefly
# WMA (Xbox), XMA (Xbox 360), ATRAC3 / ATRAC3plus (PSP, PS3), AAC, AC-3, Opus and Vorbis.
# The decoder, demuxer and parser lists are the ones vgmstream's own
# cmake/dependencies/ffmpeg.cmake uses, minus --enable-gpl: none of them needs it, so this
# build stays LGPL 2.1.
#
# Usage:
#   deps/ffmpeg/scripts/build.sh                    # auto-detect platform/arch
#   deps/ffmpeg/scripts/build.sh <platform> <arch>  # linux|mac|windows  x64|arm64, or android arm64
#
# Output: deps/ffmpeg/release/<platform>/<arch>/<version>/static/{lib,include}, which
# deps/vgmstream/scripts/build.sh picks up. CC from the environment is the cross compiler
# when set (scripts/build_native_deps.sh exports it). On Windows run it from an MSYS2
# MINGW64 shell like the other native builds. nasm is used when present (x64 SIMD); without
# it the build falls back to plain C.
#
# android/arm64 builds with the NDK's own clang (ANDROID_NDK_HOME), for API level
# ANDROID_API (default 28, what rommusic's Android app needs; see deps/rommusic/scripts/build.sh).

if [[ $# -ne 0 && $# -ne 2 ]]; then
  echo "[ERROR] Usage: $0  OR  $0 <platform> <arch>" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
user_dir="$(cd "$script_dir/.." && pwd)"
main_root="$(cd "$user_dir/../.." && pwd)"
src_dir="$(cd "${FFMPEG_SOURCE_DIR:-$main_root/external/ffmpeg}" && pwd)"
log_dir="$user_dir/logs"
mkdir -p "$log_dir"
log_file="$log_dir/build-ffmpeg-$(date +%Y%m%d-%H%M%S).log"

case "$(uname -s)" in
  Darwin) host_platform="mac" ;;
  Linux) host_platform="linux" ;;
  MINGW*|MSYS*|CYGWIN*) host_platform="windows" ;;
  *) echo "[ERROR] Unsupported OS: $(uname -s)" >&2; exit 2 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) host_arch="arm64" ;;
  x86_64|amd64) host_arch="x64" ;;
  *) echo "[ERROR] Unsupported arch: $(uname -m)" >&2; exit 2 ;;
esac
platform="${1:-$host_platform}"
arch="${2:-$host_arch}"
case "$arch" in
  x64) ff_arch="x86_64" ;;
  arm64) ff_arch="aarch64" ;;
  *) echo "[ERROR] Unsupported arch: $arch (x64 | arm64)" >&2; exit 2 ;;
esac
case "$platform" in
  windows) target_os="mingw32" ;;
  mac) target_os="darwin" ;;
  linux) target_os="linux" ;;
  android) target_os="android" ;;
  *) echo "[ERROR] Unsupported platform: $platform (linux | mac | windows | android)" >&2; exit 2 ;;
esac

decoders="ac3,eac3,wmapro,wmav1,wmav2,xma1,xma2,aac,atrac3,atrac3p,mp2float,mp3float,smackaud,binkaudio_dct,binkaudio_rdft,pcm_s16be,pcm_s16be_planar,pcm_s16le,pcm_s16le_planar,pcm_s8,pcm_s8_planar,pcm_u8,flac,vorbis,mpc7,mpc8,alac,adpcm_ima_qt,adpcm_ima_dk3,adpcm_ima_dk4,tak,ape,adpcm_swf,opus"
demuxers="ac3,eac3,spdif,asf,xwma,mov,oma,xmv,ogg,flac,wav,aac,mp3,smacker,bink,caf,mpc,mpc8,tak,ape,flv,voc"
parsers="ac3,mpegaudio,xma,vorbis,opus"

version="$(git -C "$src_dir" describe --tags --exact-match 2>/dev/null || git -C "$src_dir" log -1 --format=%cd --date=format:%Y%m%d)"
out_dir="$user_dir/release/$platform/$arch/$version/static"
build_dir="$(mktemp -d "${TMPDIR:-/tmp}/ffmpeg_build.XXXXXX")"
trap 'rm -rf "$build_dir"' EXIT

args=(
  "--prefix=$out_dir"
  --enable-static --disable-shared --enable-pic
  --disable-programs --disable-doc --disable-debug
  --disable-avdevice --disable-swscale --disable-avfilter --disable-network
  --disable-everything --enable-swresample
  --disable-autodetect --disable-iconv --disable-zlib --disable-bzlib --disable-lzma
  "--enable-decoder=$decoders" "--enable-demuxer=$demuxers" "--enable-parser=$parsers"
  "--arch=$ff_arch" "--target-os=$target_os"
)
# The symbols stay out of rvgmstream's export table: it is built with hidden visibility and
# links these archives privately, so nothing here can clash with another FFmpeg on the system.
if [[ "$platform" == "android" ]]; then
  # The NDK's clang with an explicit target rather than its aarch64-linux-android<api>-clang
  # wrapper, which on Windows is a .cmd that configure cannot run. clang finds the NDK's
  # sysroot on its own; the LLVM binutils stand in for a cross prefix the NDK does not have.
  [[ "$arch" == "arm64" ]] || { echo "[ERROR] android is arm64 only" >&2; exit 2; }
  [[ -n "${ANDROID_NDK_HOME:-}" ]] || { echo "[ERROR] ANDROID_NDK_HOME is not set" >&2; exit 2; }
  ndk_bin="$(ls -d "$ANDROID_NDK_HOME"/toolchains/llvm/prebuilt/*/bin 2>/dev/null | head -1)"
  [[ -n "$ndk_bin" ]] || { echo "[ERROR] no LLVM toolchain under $ANDROID_NDK_HOME/toolchains/llvm/prebuilt" >&2; exit 2; }
  exe=""; [[ -f "$ndk_bin/clang.exe" ]] && exe=".exe"
  android_target="--target=aarch64-linux-android${ANDROID_API:-28}"
  args+=(--enable-cross-compile "--cc=$ndk_bin/clang$exe" "--cxx=$ndk_bin/clang++$exe"
    "--ar=$ndk_bin/llvm-ar$exe" "--nm=$ndk_bin/llvm-nm$exe" "--ranlib=$ndk_bin/llvm-ranlib$exe" "--strip=$ndk_bin/llvm-strip$exe"
    "--extra-cflags=$android_target" "--extra-ldflags=$android_target")
  CC="$ndk_bin/clang$exe"
elif [[ -n "${CC:-}" ]]; then
  args+=("--cc=$CC")
fi
if [[ "$platform" != "android" ]] && [[ "$platform" != "$host_platform" || "$arch" != "$host_arch" ]]; then
  args+=(--enable-cross-compile)
  if [[ -n "${CC:-}" ]]; then
    prefix="${CC%gcc}"; prefix="${prefix%clang}"
    args+=("--cross-prefix=$prefix")
  fi
fi
if [[ "$ff_arch" == "x86_64" ]] && ! command -v nasm >/dev/null 2>&1; then
  echo "[WARN] nasm not found: building FFmpeg without its x86 assembly"
  args+=(--disable-x86asm)
fi

jobs="${JOBS:-$( (command -v nproc >/dev/null 2>&1 && nproc) || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"

rm -rf "$out_dir"
{
  echo "[INFO] ffmpeg $version $platform/$arch -> $out_dir"
  cd "$build_dir"
  "$src_dir/configure" "${args[@]}"
  make -j "$jobs"
  make install
} 2>&1 | tee "$log_file"

for lib in avformat avcodec swresample avutil; do
  [[ -f "$out_dir/lib/lib$lib.a" ]] || { echo "[ERROR] lib$lib.a was not produced; see $log_file" >&2; exit 1; }
done
rm -rf "$out_dir/lib/pkgconfig" "$out_dir/share"
{
  echo "platform=$platform"
  echo "arch=$arch"
  echo "version=$version"
  echo "commit=$(git -C "$src_dir" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "compiler=${CC:-default}"
  echo "built=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$out_dir/build-info.txt"
echo "[INFO] built $out_dir"
