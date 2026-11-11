#!/usr/bin/env bash
set -euo pipefail

# Builds rommusic.dll / librommusic.so / librommusic.dylib: the players for music made by a
# console's own sound chips, in one library (see deps/rommusic/CMakeLists.txt):
#   - upstream libgme (LGPL-2.1+) for rip sets: NSF/NSFE, SPC, GBS, VGM/VGZ, GYM, HES, KSS, AY, SAP;
#   - SGC (Game Gear, Master System) rips from kode54's fork (external/gme-kode54), its names
#     prefixed by sgc/rename.h so they do not collide with upstream's; deps/rommusic/patches/
#     is applied to the fork first;
#   - the SNES sound chip driven through its ports (rspc_);
#   - agbplay's playback core for GBA music from the ROM (LGPL-3.0, C++23);
#   - kode54's SSEQ Player for DS sequenced music (external/SSEQPlayer, WTFPL), with
#     deps/rommusic/patches/sseq/ applied first;
#   - lazyusf2 for N64 USF rips (external/lazyusf2, GPL-2.0+);
#   - Highly Experimental for PS1/PS2 PSF rips (external/highly_experimental, GPL-3.0);
#   - Highly Theoretical for Saturn/Dreamcast SSF/DSF rips (external/highly_theoretical, GPL-3.0);
#   - mGBA's GBA core for GSF rips (external/mgba, MPL-2.0);
#   - Beetle WonderSwan for WSR rips (external/beetle-wswan-libretro, GPL-2.0+).
#   - libvgm for Neo Geo Pocket and Virtual Boy VGM logs (external/libvgm; SN76496 and VSU cores),
#     and its SN76496, emu2413 (YM2413, MIT) and Nuked OPN2 (YM2612, LGPL-2.1+) cores driven a
#     write at a time (rchip_).
#   - VGMTrans's core (external/vgmtrans, zlib licence) and TinySoundFont (external/TinySoundFont,
#     MIT) for sequenced music read from a disc's own files (rvt_).
#
# Usage:
#   deps/rommusic/scripts/build.sh                    # auto-detect platform/arch
#   deps/rommusic/scripts/build.sh <platform> <arch>  # linux|mac|windows  x64|arm64, or android arm64
#
# Output: deps/rommusic/release/<platform>/<arch>/<version>/dynamic/<library>, the layout
# scripts/move_libs_to_app_root.sh and src/rom/native_lib.rs expect.
#
# Options fixed here, and why:
#   - Nuked YM2612 (LGPL), not MAME's.
#   - No zlib of our own: .vgz is gunzipped on the Rust side through rom::zlib_ng. VGMTrans's
#     vendored zlib is linked in statically for its own loaders, so there is still nothing
#     to deploy beside the library.
#   - On Windows the MinGW runtime (libstdc++, libgcc, winpthread) is linked in statically, so
#     the DLL imports only system DLLs.
#
# The compiler is CXX/CC from the environment when set (scripts/build_native_deps.sh exports the
# cross compiler that way), else the host's default. On Windows run it from an MSYS2 MINGW64
# shell, like the other native builds. agbplay needs a C++23 compiler (GCC 13 or Clang 17).

if [[ $# -ne 0 && $# -ne 2 ]]; then
  echo "[ERROR] Usage: $0  OR  $0 <platform> <arch>" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
user_dir="$(cd "$script_dir/.." && pwd)"
main_root="$(cd "$user_dir/../.." && pwd)"
repo_root="$(cd "${GME_SOURCE_DIR:-$main_root/external/game-music-emu}" && pwd)"
fork_root="$(cd "${GME_FORK_DIR:-$main_root/external/gme-kode54}" && pwd)"
agb_root="$(cd "${AGBPLAY_SOURCE_DIR:-$main_root/external/agbplay}" && pwd)"
sseq_root="$(cd "${SSEQ_SOURCE_DIR:-$main_root/external/SSEQPlayer}" && pwd)"
usf_root="$(cd "${USF_SOURCE_DIR:-$main_root/external/lazyusf2}" && pwd)"
he_root="$(cd "${HE_SOURCE_DIR:-$main_root/external/highly_experimental}" && pwd)"
ht_root="$(cd "${HT_SOURCE_DIR:-$main_root/external/highly_theoretical}" && pwd)"
mgba_root="$(cd "${MGBA_SOURCE_DIR:-$main_root/external/mgba}" && pwd)"
wswan_root="$(cd "${WSWAN_SOURCE_DIR:-$main_root/external/beetle-wswan-libretro}" && pwd)"
vgm_root="$(cd "${VGM_SOURCE_DIR:-$main_root/external/libvgm}" && pwd)"
vgmtrans_root="$(cd "${VGMTRANS_SOURCE_DIR:-$main_root/external/vgmtrans}" && pwd)"
tsf_root="$(cd "${TSF_SOURCE_DIR:-$main_root/external/TinySoundFont}" && pwd)"
log_dir="$user_dir/logs"
mkdir -p "$log_dir"
log_file="$log_dir/build-rommusic-$(date +%Y%m%d-%H%M%S).log"

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
  windows) lib_name="rommusic.dll"; system_name="Windows"; built_glob="*rommusic.dll" ;;
  mac) lib_name="librommusic.dylib"; system_name="Darwin"; built_glob="librommusic*.dylib" ;;
  linux) lib_name="librommusic.so"; system_name="Linux"; built_glob="librommusic.so*" ;;
  android) lib_name="librommusic.so"; system_name="Android"; built_glob="librommusic.so*" ;;
  *) echo "[ERROR] Unsupported platform: $platform (linux | mac | windows | android)" >&2; exit 2 ;;
esac

command -v cmake >/dev/null 2>&1 || { echo "[ERROR] cmake was not found." >&2; exit 2; }

# The release tag (0.6.5 ...) when the checkout sits on one, else the commit date.
output_version="$(git -C "$repo_root" describe --tags --exact-match 2>/dev/null || git -C "$repo_root" log -1 --format=%cd --date=format:%Y%m%d 2>/dev/null || date +%Y%m%d)"
out_dir="$user_dir/release/$platform/$arch/$output_version"
build_dir="$(mktemp -d "${TMPDIR:-/tmp}/rommusic_build.XXXXXX")"
trap '[ -n "${KEEP_BUILD:-}" ] || rm -rf "$build_dir"' EXIT

# Our fixes, applied to the checkouts idempotently (the 7-Zip scheme): patches/*.patch to the
# fork, patches/sseq/, usf/ and agb/ to the SSEQ Player, lazyusf2 and agbplay. They show as modified in `git status` inside
# those submodules afterwards; that is expected.
apply_patches() {
  # The patches of a directory are a series, applied in name order. Later ones may touch what
  # earlier ones did, so whether the series is applied is told from the last one that undoes
  # cleanly: those up to it are in, the rest are applied.
  local dir="$1" root="$2" p i done_upto=-1
  local series=()
  for p in "$dir"/*.patch; do [[ -e "$p" ]] && series+=("$p"); done
  # Match each hunk's line endings to the file it patches. GNU patch refuses an LF hunk
  # against CRLF lines ("different line endings"), and which files are CRLF depends on the
  # checkout: lazyusf2's usf.c is CRLF upstream, and core.autocrlf on Windows makes the rest
  # CRLF too. The patches themselves are stored with LF (or as made); copies go to build_dir.
  local eol_dir="$build_dir/patches/$(basename "$dir")"
  mkdir -p "$eol_dir"
  for ((i = 0; i < ${#series[@]}; i++)); do
    p="${series[$i]}"
    local crlf_files="" f
    for f in $(tr -d '\r' <"$p" | sed -n 's|^+++ b/||p'); do
      grep -q $'\r$' "$root/$f" 2>/dev/null && crlf_files+=" $f"
    done
    tr -d '\r' <"$p" | awk -v crlf="$crlf_files " '
      /^diff --git / { body = 0; print; next }
      /^\+\+\+ b\// { f = substr($0, 7); cr = index(crlf, " " f " ") > 0; print; next }
      /^@@ / { body = 1; print; next }
      body && cr && /^[ +-]/ { print $0 "\r"; next }
      { print }' >"$eol_dir/$(basename "$p")"
    series[$i]="$eol_dir/$(basename "$p")"
  done
  for ((i = ${#series[@]} - 1; i >= 0; i--)); do
    if (cd "$root" && patch -p1 -R -f --dry-run <"${series[$i]}") >/dev/null 2>&1; then
      done_upto=$i
      break
    fi
  done
  for ((i = 0; i < ${#series[@]}; i++)); do
    p="${series[$i]}"
    if ((i <= done_upto)); then
      echo "[INFO] patch already applied: $(basename "$p")" | tee -a "$log_file"
    elif (cd "$root" && patch -p1 -N -f --dry-run <"$p") >/dev/null 2>&1; then
      (cd "$root" && patch -p1 -N -f <"$p") | tee -a "$log_file"
      echo "[INFO] applied patch: $(basename "$p")" | tee -a "$log_file"
    else
      echo "[ERROR] patch $(basename "$p") does not apply to $root (forward or reverse) -- upstream moved; refresh the patch" | tee -a "$log_file" >&2
      exit 1
    fi
  done
}
apply_patches "$user_dir/patches" "$fork_root"
apply_patches "$user_dir/patches/sseq" "$sseq_root"
apply_patches "$user_dir/patches/usf" "$usf_root"
apply_patches "$user_dir/patches/agb" "$agb_root"
apply_patches "$user_dir/patches/ht" "$ht_root"

cmake_args=(-S "$user_dir" -B "$build_dir" -DCMAKE_BUILD_TYPE=Release "-DCMAKE_SYSTEM_PROCESSOR=$processor"
  "-DGME_ROOT=$repo_root" "-DGME_FORK_ROOT=$fork_root" "-DAGBPLAY_ROOT=$agb_root" "-DSSEQ_ROOT=$sseq_root" "-DUSF_ROOT=$usf_root" "-DHE_ROOT=$he_root" "-DHT_ROOT=$ht_root" "-DMGBA_ROOT=$mgba_root" "-DWSWAN_ROOT=$wswan_root" "-DVGM_ROOT=$vgm_root" "-DVGMTRANS_ROOT=$vgmtrans_root" "-DTSF_ROOT=$tsf_root")
[[ -n "${CC:-}" ]] && cmake_args+=("-DCMAKE_C_COMPILER=$CC")
[[ -n "${CXX:-}" ]] && cmake_args+=("-DCMAKE_CXX_COMPILER=$CXX")
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
  if command -v ninja >/dev/null 2>&1; then cmake_args+=(-G Ninja); else cmake_args+=(-G "MinGW Makefiles"); fi
fi

jobs="${JOBS:-$( (command -v nproc >/dev/null 2>&1 && nproc) || sysctl -n hw.ncpu 2>/dev/null || echo 4)}"

{
  echo "[INFO] rommusic $platform/$arch -> $out_dir"
  echo "[INFO] cmake ${cmake_args[*]}"
  cmake "${cmake_args[@]}"
  cmake --build "$build_dir" --target rommusic -j "$jobs"
} 2>&1 | tee -a "$log_file"

# The real file, not a SONAME symlink. MinGW names it librommusic.dll; the app looks for
# rommusic.dll, like every other library it loads.
built="$(find "$build_dir" -name "$built_glob" -type f ! -name '*.a' | head -1)"
if [[ -z "$built" ]]; then
  echo "[ERROR] the rommusic library was not produced; see $log_file" >&2
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
  echo "fork_commit=$(git -C "$fork_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "agbplay_commit=$(git -C "$agb_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "sseq_commit=$(git -C "$sseq_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "lazyusf2_commit=$(git -C "$usf_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "highly_experimental_commit=$(git -C "$he_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "highly_theoretical_commit=$(git -C "$ht_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "mgba_commit=$(git -C "$mgba_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "beetle_wswan_commit=$(git -C "$wswan_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "libvgm_commit=$(git -C "$vgm_root" rev-parse HEAD 2>/dev/null || echo unknown)"
  echo "compiler=${CXX:-default}"
  echo "built=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
} > "$out_dir/build-info.txt"
echo "[INFO] built $out_dir/dynamic/$lib_name ($(wc -c < "$out_dir/dynamic/$lib_name") bytes)"
