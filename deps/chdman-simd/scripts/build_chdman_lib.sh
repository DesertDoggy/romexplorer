#!/usr/bin/env bash
set -euo pipefail

# Builds chdman as a shared library (chdman.dll / libchdman.so / libchdman.dylib) for one
# platform/arch target, with chdman-simd's SIMD/pipeline patches applied and every
# dependency (LZMA, zstd, expat, utf8proc, FLAC, zlib-ng) statically linked in.
#
# All build output lives under deps/chdman-simd/ -- nothing is ever
# written into the mame/ or chdman-simd repo trees themselves, so both stay pristine
# submodule checkouts.
#
# Usage:
#   deps/chdman-simd/scripts/build_chdman_lib.sh                      # auto-detect host platform/arch
#   deps/chdman-simd/scripts/build_chdman_lib.sh <platform> <arch>
#   deps/chdman-simd/scripts/build_chdman_lib.sh --nostreaming [<platform> <arch>]
#
# Builds the streaming variant by default (--nostreaming builds the plain one; --streaming
# is still accepted, as a no-op). The streaming variant additionally applies
# deps/chdman-simd/patches/streaming/*.patch (write-site hook in chdman.cpp's extract loops) and
# compiles deps/chdman-simd/chdman_stream.cpp (chdman_extract_stream + the chdman_reader_* random
# access API, see chdman_lib.h) with -DCHDMAN_WITH_STREAMING. It uses its own scratch
# source/object dirs under deps/chdman-simd/_build/with_streaming/ and installs to
# deps/chdman-simd/release/with_streaming/<platform>/<arch>/<version>/, so both variants coexist and
# the default build's sources, flags, exports and output path are unchanged.
#
# Targets:
#   windows/x64   linux/x64   mac/arm64   android/arm64   ios/arm64
#
# Cross-compiling (android/ios, or windows/linux from a different host) requires the
# right toolchain already installed -- see the per-platform notes below and
# deps/chdman-simd/docs/README.chdman-lib.md.

streaming=1
args=()
for arg in "$@"; do
  case "$arg" in
    --streaming) streaming=1 ;;
    --nostreaming) streaming=0 ;;
    *) args+=("$arg") ;;
  esac
done
set -- ${args[@]+"${args[@]}"}
# Per-variant path segment under deps/chdman-simd/_build and deps/chdman-simd/release. A separate scratch source
# and object dir per variant is mandatory, not tidiness: Makefile.chdman_lib has no
# header/flag dependency tracking, so a shared BUILDDIR would silently link objects
# compiled for the other variant.
variant_dir=""
(( streaming )) && variant_dir="with_streaming/"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
user_dir="$(cd "$script_dir/.." && pwd)"
main_root="$(cd "$user_dir/../.." && pwd)"
# Upstream sources, all submodules under external/: chdman-simd's own patches, MAME and
# zlib-ng. This script, our wrapper sources/patches and the build output live in user_dir
# (deps/chdman-simd/). Each can be overridden, e.g. to build against another checkout.
submodule_root="${CHDMAN_SIMD_SOURCE_DIR:-$main_root/external/chdman-simd}"
mame_src="${MAME_SOURCE_DIR:-$main_root/external/mame}"
zlibng_src="${ZLIBNG_SOURCE_DIR:-$main_root/external/zlib-ng}"

# Only MAME's git history is read (the sources come out through `git archive`), so a clone
# without a checked-out tree works too.
if ! git -C "$mame_src" rev-parse --git-dir >/dev/null 2>&1; then
  echo "[ERROR] MAME source not found at: $mame_src" >&2
  echo "[ERROR] Expected the 'mame' submodule at external/mame." >&2
  exit 2
fi
if [[ ! -d "$zlibng_src" ]]; then
  echo "[ERROR] zlib-ng source not found at: $zlibng_src" >&2
  exit 2
fi

if [[ $# -eq 0 ]]; then
  os_name="$(uname -s)"
  cpu_name="$(uname -m)"
  case "$os_name" in
    Darwin) platform="mac" ;;
    Linux) platform="linux" ;;
    MINGW*|MSYS*|CYGWIN*) platform="windows" ;;
    *) echo "[ERROR] Unsupported OS for auto-detect: $os_name" >&2; exit 2 ;;
  esac
  case "$cpu_name" in
    arm64|aarch64) arch="arm64" ;;
    x86_64|amd64) arch="x64" ;;
    *) echo "[ERROR] Unsupported arch for auto-detect: $cpu_name" >&2; exit 2 ;;
  esac
elif [[ $# -eq 2 ]]; then
  platform="$1"
  arch="$2"
else
  echo "[ERROR] Usage: $0 [--nostreaming] OR $0 [--nostreaming] <platform> <arch>" >&2
  exit 2
fi

case "$platform/$arch" in
  windows/x64|windows/arm64|linux/x64|linux/arm64|mac/arm64|android/arm64|ios/arm64) ;;
  *)
    echo "[ERROR] Unsupported platform/arch combination: $platform/$arch" >&2
    echo "[ERROR] Supported: windows/x64 windows/arm64 linux/x64 linux/arm64 mac/arm64 android/arm64 ios/arm64" >&2
    exit 2
    ;;
esac

# ---------------------------------------------------------------------------
# chdman-simd's own version tag (e.g. "0.286-1-g08ea73e") -- used only for the release
# output path; if git describe finds no tag (a fresh clone of the fork has none) it falls back to a
# build datetime, which still reads as a version directory to move_libs_to_app_root.sh.
# ---------------------------------------------------------------------------
version="$(git -C "$submodule_root" describe --tags 2>/dev/null || date +%Y%m%d-%H%M%S)"

log_dir="$user_dir/logs"
mkdir -p "$log_dir"
log_file="$log_dir/build-chdman-$platform-$arch-$(date +%Y%m%d-%H%M%S).log"

echo "[INFO] platform=$platform arch=$arch version=$version streaming=$streaming" | tee -a "$log_file"

# ---------------------------------------------------------------------------
# Toolchain selection per target
# ---------------------------------------------------------------------------
MARCH_EXTRA=""
HAVE_LZMA_ASM=0
# Set for the cross targets below, so the zlib-ng sub-build uses the same compiler.
zlibng_cross_args=()

case "$platform" in
  windows)
  if [[ "$arch" == "arm64" ]]; then
    # MSYS2's MinGW64 gcc has no aarch64 target; llvm-mingw's clang does, and speaks the
    # same GNU driver flags Makefile.chdman_lib uses (-static-libgcc/-static-libstdc++
    # included). No LZMA ASM: the uasm path is x86_64 asm.
    CC="${CC_WINDOWS_ARM64:-aarch64-w64-mingw32-clang}"
    CXX="${CXX_WINDOWS_ARM64:-aarch64-w64-mingw32-clang++}"
    AR="${AR_WINDOWS_ARM64:-llvm-ar}"
    command -v "$CXX" >/dev/null 2>&1 || { echo "[ERROR] $CXX not found. Put llvm-mingw's bin/ on PATH (https://github.com/mstorsjo/llvm-mingw)." >&2; exit 3; }
    zlibng_cross_args=(-DCMAKE_SYSTEM_NAME=Windows -DCMAKE_SYSTEM_PROCESSOR=aarch64
      -DCMAKE_C_COMPILER="$CC" -DCMAKE_AR="$(command -v "$AR")")
  else
    CC=gcc
    CXX=g++
    AR=ar
    if ! command -v "$CXX" >/dev/null 2>&1; then
      echo "[ERROR] g++ not found. Run from an MSYS2 MinGW64 shell (pacman -S mingw-w64-x86_64-gcc)." >&2
      exit 3
    fi
    # Different UASM distributions name the binary differently (uasm.exe vs the
    # UEFI-targeted build's uasm64.exe) -- check all known names. MSYS2's MinGW64
    # shell launches with a deliberately minimal PATH (no C:\ tool directories), so
    # also check common install locations directly even when PATH doesn't have them.
    UASM_BIN=""
    for candidate in uasm64 uasm64.exe uasm uasm.exe; do
      if command -v "$candidate" >/dev/null 2>&1; then
        UASM_BIN="$(command -v "$candidate")"
        break
      fi
    done
    if [[ -z "$UASM_BIN" ]]; then
      for candidate in "/c/uasm/uasm64.exe" "${UASM_HOME:-}/uasm64.exe" "/c/uasm/uasm.exe"; do
        if [[ -n "$candidate" && -f "$candidate" ]]; then
          UASM_BIN="$candidate"
          break
        fi
      done
    fi
    if [[ -n "$UASM_BIN" ]]; then
      HAVE_LZMA_ASM=1
      echo "[INFO] uasm found at $UASM_BIN -- LZMA ASM decoder enabled" | tee -a "$log_file"
    else
      echo "[INFO] uasm not found on PATH -- LZMA ASM decoder disabled (C fallback)" | tee -a "$log_file"
    fi
  fi
    ;;
  linux)
    case "$(uname -m)" in arm64|aarch64) host_is_arm64=1 ;; *) host_is_arm64=0 ;; esac
    if [[ "$arch" == "arm64" ]] && (( ! host_is_arm64 )); then
      # GNU aarch64 cross toolchain (apt: g++-aarch64-linux-gnu).
      prefix="${LINUX_ARM64_CROSS_PREFIX:-aarch64-linux-gnu-}"
      CC="${prefix}gcc"; CXX="${prefix}g++"; AR="${prefix}ar"
      command -v "$CXX" >/dev/null 2>&1 || { echo "[ERROR] $CXX not found. Install g++-aarch64-linux-gnu." >&2; exit 3; }
      zlibng_cross_args=(-DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=aarch64
        -DCMAKE_C_COMPILER="$CC" -DCMAKE_AR="$(command -v "$AR")")
    else
      CC="${CC:-gcc}"
      CXX="${CXX:-g++}"
      AR="${AR:-ar}"
    fi
    ;;
  mac)
    CC=clang
    CXX=clang++
    AR=ar
    MARCH_EXTRA="-arch arm64"
    ;;
  android)
    : "${ANDROID_NDK_HOME:?[ERROR] ANDROID_NDK_HOME must point to an installed Android NDK (r26+)}"
    host_tag=""
    case "$(uname -s)" in
      Darwin) host_tag="darwin-x86_64" ;;
      Linux) host_tag="linux-x86_64" ;;
      MINGW*|MSYS*|CYGWIN*) host_tag="windows-x86_64" ;;
    esac
    ndk_bin="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$host_tag/bin"
    android_api=21
    CC="$ndk_bin/aarch64-linux-android${android_api}-clang"
    CXX="$ndk_bin/aarch64-linux-android${android_api}-clang++"
    AR="$ndk_bin/llvm-ar"
    [[ -x "$CC" ]] || { echo "[ERROR] NDK clang not found: $CC" >&2; exit 3; }
    ;;
  ios)
    command -v xcrun >/dev/null 2>&1 || { echo "[ERROR] xcrun not found (requires Xcode)" >&2; exit 3; }
    sdk_path="$(xcrun --sdk iphoneos --show-sdk-path)"
    CC="xcrun --sdk iphoneos clang"
    CXX="xcrun --sdk iphoneos clang++"
    AR=ar
    MARCH_EXTRA="-arch arm64 -isysroot $sdk_path -mios-version-min=13.0"
    echo "[WARN] iOS target is experimental -- Dolphin/MAME upstream have no official" | tee -a "$log_file"
    echo "[WARN] iOS support; this is chdman only (no JIT/emulation core needed)." | tee -a "$log_file"
    ;;
esac

# ---------------------------------------------------------------------------
# Scratch source copy: MAME src/ + curated 3rdparty/, with chdman-simd's patches
# applied. Lives entirely under deps/chdman-simd/_build -- never touches mame/ or chdman-simd/'s
# own tracked files.
#
# Source is extracted via `git archive` at whatever MAME commit chdman-simd's own docs
# say its patches target (parsed from patches/APPLY_PATCHES.md's "Generated from MAME
# **X.YYY**"), NOT from external/mame's currently checked-out commit. Both mame/ and
# chdman-simd/ are submodules the user updates on their own schedule (tracking newer
# MAME releases, newer chdman-simd patch sets); this keeps the build correct regardless
# of what commit mame/ happens to be sitting on, as long as that release's history is
# present in mame/'s git objects (it must have been fetched at some point -- e.g. by
# ever having been checked out, or by `git fetch --tags` -- but need not be HEAD now).
# ---------------------------------------------------------------------------
src_copy="$user_dir/_build/${variant_dir}$platform/$arch/src"
rm -rf "$src_copy"
mkdir -p "$src_copy"

patches_doc="$submodule_root/patches/APPLY_PATCHES.md"
[[ -f "$patches_doc" ]] || { echo "[ERROR] $patches_doc not found -- can't determine required MAME version" >&2; exit 3; }
mame_version="$(sed -n 's/.*Generated from MAME \*\*\([0-9][0-9.]*\)\*\*.*/\1/p' "$patches_doc" | head -1)"
[[ -n "$mame_version" ]] || { echo "[ERROR] Could not parse required MAME version from $patches_doc" >&2; exit 3; }

# MAME's own release process consistently commits a single "Bumped version to X.YYY"
# right at each release point (confirmed against this repo's actual history) -- search
# every ref, not just whatever's currently checked out, since the needed commit may not
# be reachable from mame/'s current HEAD.
mame_commit="$(git -C "$mame_src" log --all --format='%H %s' 2>/dev/null | grep -i "Bumped version to ${mame_version}\$" | head -1 | cut -d' ' -f1)"
if [[ -z "$mame_commit" ]]; then
  echo "[ERROR] No 'Bumped version to $mame_version' commit found in external/mame's history." >&2
  echo "[ERROR] chdman-simd's patches target MAME $mame_version -- fetch that release into" >&2
  echo "[ERROR] external/mame (it doesn't need to be checked out) and try again." >&2
  exit 3
fi
echo "[INFO] chdman-simd targets MAME $mame_version -> using commit $mame_commit" | tee -a "$log_file"

echo "[INFO] Extracting MAME src/ at $mame_commit ..." | tee -a "$log_file"
git -C "$mame_src" archive "$mame_commit" -- src | tar -x -C "$src_copy"

if [[ ! -f "$src_copy/src/version.cpp" ]]; then
  cat > "$src_copy/src/version.cpp" <<EOF
#define BARE_BUILD_VERSION "$mame_version"
#define BARE_VCS_REVISION "$mame_commit"
extern const char bare_build_version[];
extern const char bare_vcs_revision[];
extern const char build_version[];
const char bare_build_version[] = BARE_BUILD_VERSION;
const char bare_vcs_revision[]  = BARE_VCS_REVISION;
const char build_version[]      = BARE_BUILD_VERSION " (" BARE_VCS_REVISION ")";
EOF
fi

# Exact subpaths actually needed -- not whole library trees. This matters beyond
# leanness: 3rdparty/zstd's tests/ subtree (irrelevant to the build) contains a symlink,
# which `tar -x` can't create on Windows without elevated privileges (confirmed
# directly), aborting the whole extraction. Narrowing to just what's needed avoids
# pulling that in at all.
#
# aes256cbc/nanosvg ARE needed despite not appearing in Makefile.chdman_lib's own -I/
# *_CSRCS lists (confirmed directly, the hard way): src/lib/util/aes256cbc.cpp and
# nanosvg.cpp are thin wrapper files (part of UTIL_SRCS, always compiled) that
# `#include <aes256cbc/AES_256_CBC.h>` / `#include <nanosvg/src/nanosvg.h>` -- resolved
# via the Makefile's generic `-I3rdparty` root, not an aes256cbc/nanosvg-specific -I, so
# grepping the Makefile for explicit $(TP)/aes256cbc-style references misses this
# dependency entirely. Omitting them doesn't fail the extraction or the configure step --
# it fails silently later, deep in a parallel build, as a g++ error that (because
# Makefile.chdman_lib redirects warnings, but ends up catching real errors too, to
# build/build.log) never reaches the terminal at all.
echo "[INFO] Extracting 3rdparty/ at $mame_commit ..." | tee -a "$log_file"
mkdir -p "$src_copy/3rdparty"
for subpath in \
  lzma/C \
  lzma/Asm \
  utf8proc \
  expat/lib \
  zstd/lib \
  flac/include \
  flac/src \
  aes256cbc \
  nanosvg
do
  if git -C "$mame_src" cat-file -e "$mame_commit:3rdparty/$subpath" 2>/dev/null; then
    git -C "$mame_src" archive "$mame_commit" -- "3rdparty/$subpath" | tar -x -C "$src_copy"
  fi
done

echo "[INFO] Copying wrapper sources (user/chdman_lib.*, user/osdlib_posix_min.cpp) ..." | tee -a "$log_file"
mkdir -p "$src_copy/user"
cp "$user_dir/chdman_lib.h" "$user_dir/chdman_lib.cpp" "$user_dir/osdlib_posix_min.cpp" "$src_copy/user/"
if (( streaming )); then
  cp "$user_dir/chdman_stream.cpp" "$user_dir/chdman_stream_hook.h" "$src_copy/user/"
fi

echo "[INFO] Applying chdman-simd patches ..." | tee -a "$log_file"
cd "$src_copy"
for patch in "$submodule_root"/patches/0*.patch; do
  name="$(basename "$patch")"
  if patch -p1 --dry-run < "$patch" >/dev/null 2>&1; then
    patch -p1 < "$patch"
    echo "    [OK] $name" | tee -a "$log_file"
  elif patch -p1 --fuzz=5 --dry-run < "$patch" >/dev/null 2>&1; then
    patch -p1 --fuzz=5 < "$patch"
    echo "    [OK] $name (with fuzz)" | tee -a "$log_file"
  else
    echo "    [FAIL] $name -- manual merge required (see chdman-simd/patches/APPLY_PATCHES.md)." | tee -a "$log_file"
    echo "    [FAIL] This means MAME $mame_version's source itself doesn't match what chdman-simd" | tee -a "$log_file"
    echo "    [FAIL] expects, despite being the commit its own docs say to target -- check whether" | tee -a "$log_file"
    echo "    [FAIL] chdman-simd's patches or its version claim in patches/APPLY_PATCHES.md are stale." | tee -a "$log_file"
    exit 4
  fi
done

# arm64 patches -- ours, kept in deps/chdman-simd/ like the streaming ones below. chdman-simd's own
# patches add an x86-only cpuid/xgetbv SIMD report to chdman.cpp with no architecture
# guard; these fence it off, before the streaming patches so their context never depends
# on which variant is being built.
if [[ "$arch" == "arm64" ]]; then
  echo "[INFO] Applying arm64 patches (deps/chdman-simd/patches/arm64) ..." | tee -a "$log_file"
  for patch in "$user_dir"/patches/arm64/*.patch; do
    name="$(basename "$patch")"
    if patch -p1 --dry-run < "$patch" >/dev/null 2>&1; then
      patch -p1 < "$patch"
      echo "    [OK] $name" | tee -a "$log_file"
    else
      echo "    [FAIL] $name -- chdman.cpp moved under chdman-simd's patches; regenerate it" | tee -a "$log_file"
      exit 4
    fi
  done
fi

# Streaming hook patches -- ours, kept in deps/chdman-simd/ (never in chdman-simd's own patches/), and
# applied on top of chdman-simd's so their context is the already-patched chdman.cpp.
if (( streaming )); then
  echo "[INFO] Applying streaming patches (deps/chdman-simd/patches/streaming) ..." | tee -a "$log_file"
  for patch in "$user_dir"/patches/streaming/*.patch; do
    name="$(basename "$patch")"
    if patch -p1 --dry-run < "$patch" >/dev/null 2>&1; then
      patch -p1 < "$patch"
      echo "    [OK] $name" | tee -a "$log_file"
    else
      echo "    [FAIL] $name -- chdman.cpp's extract write sites moved; regenerate the patch" | tee -a "$log_file"
      echo "    [FAIL] against a scratch copy with chdman-simd's patches applied." | tee -a "$log_file"
      exit 4
    fi
  done
fi

# ---------------------------------------------------------------------------
# zlib-ng (ZLIB_COMPAT=ON), built once per platform/arch from the zlib-ng submodule.
# ---------------------------------------------------------------------------
zlibng_build="$user_dir/_build/$platform/$arch/zlib-ng"
zlibng_lib="$zlibng_build/libz.a"

if [[ ! -f "$zlibng_lib" ]]; then
  echo "[INFO] Building zlib-ng ..." | tee -a "$log_file"
  cmake_generator="Unix Makefiles"
  [[ "$platform" == "windows" ]] && cmake_generator="MinGW Makefiles"

  cmake_extra_args=()
  case "$platform" in
    mac)
      cmake_extra_args+=(-DCMAKE_OSX_ARCHITECTURES=arm64)
      ;;
    ios)
      cmake_extra_args+=(-DCMAKE_SYSTEM_NAME=iOS -DCMAKE_OSX_ARCHITECTURES=arm64
        -DCMAKE_OSX_DEPLOYMENT_TARGET=13.0)
      ;;
    android)
      cmake_extra_args+=(-DCMAKE_TOOLCHAIN_FILE="$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake"
        -DANDROID_ABI=arm64-v8a -DANDROID_PLATFORM=android-21)
      ;;
  esac

  cmake -S "$zlibng_src" -B "$zlibng_build" -G "$cmake_generator" \
    -DZLIB_COMPAT=ON -DBUILD_SHARED_LIBS=OFF -DZLIB_ENABLE_TESTS=OFF -DWITH_GTEST=OFF \
    -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
    -DCMAKE_C_FLAGS="-O3 $MARCH_EXTRA" \
    "${cmake_extra_args[@]}" ${zlibng_cross_args[@]+"${zlibng_cross_args[@]}"} 2>&1 | tee -a "$log_file"
  # --target zlib-ng only (produces libz.a) -- the default "all" target also builds
  # zlib-ng's own dev/test utility executables (utils/CMakeLists.txt's maketrees.exe,
  # makefixed.exe, minigzip.exe, etc), which aren't needed for the static lib chdman
  # actually links (they only feed zlib-ng's own test suite, already disabled above via
  # ZLIB_ENABLE_TESTS=OFF -- but utils/ itself still builds unconditionally regardless of
  # that flag). Skipping them avoids some freshly-built, unsigned .exe files that some
  # antivirus (confirmed: Norton) flags/quarantines on sight, which was also intermittently
  # breaking the build outright (a locked/quarantined maketrees.exe made the subsequent
  # link step fail with "Permission denied").
  cmake --build "$zlibng_build" --target zlib-ng -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)" 2>&1 | tee -a "$log_file"
fi

[[ -f "$zlibng_lib" ]] || { echo "[ERROR] zlib-ng build did not produce $zlibng_lib" >&2; exit 5; }

# ---------------------------------------------------------------------------
# Build chdman shared library
# ---------------------------------------------------------------------------
build_obj_dir="$user_dir/_build/${variant_dir}$platform/$arch/obj"
out_dir="$user_dir/release/${variant_dir}$platform/$arch/$version/dynamic"
include_dir="$user_dir/release/${variant_dir}$platform/$arch/$version/include"
mkdir -p "$out_dir" "$include_dir"

echo "[INFO] Building chdman ($platform/$arch) ..." | tee -a "$log_file"
cd "$src_copy"
make -f "$user_dir/Makefile.chdman_lib" \
  PLATFORM="$platform" ARCH="$arch" \
  CC="$CC" CXX="$CXX" AR="$AR" \
  MARCH_EXTRA="$MARCH_EXTRA" \
  HAVE_LZMA_ASM="$HAVE_LZMA_ASM" \
  UASM_BIN="${UASM_BIN:-}" \
  ZLIBNG_BUILD="$zlibng_build" \
  BUILDDIR="$build_obj_dir" \
  STREAMING="$streaming" \
  -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)" 2>&1 | tee -a "$log_file"

case "$platform" in
  windows) built_name="chdman.dll" ;;
  mac|ios) built_name="libchdman.dylib" ;;
  *) built_name="libchdman.so" ;;
esac

[[ -f "$src_copy/$built_name" ]] || { echo "[ERROR] Build did not produce $src_copy/$built_name" | tee -a "$log_file"; exit 6; }

cp -f "$src_copy/$built_name" "$out_dir/$built_name"
cp -f "$user_dir/chdman_lib.h" "$include_dir/chdman_lib.h"

echo "" | tee -a "$log_file"
echo "[INFO] Build complete." | tee -a "$log_file"
echo "[INFO] Library : $out_dir/$built_name" | tee -a "$log_file"
echo "[INFO] Header  : $include_dir/chdman_lib.h" | tee -a "$log_file"
