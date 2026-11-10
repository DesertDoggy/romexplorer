#!/usr/bin/env bash
set -euo pipefail

# Resolve paths from this script's own location (deps/RALibretro), not from the caller's
# working directory, so it can be invoked from anywhere. The sources are the upstream
# submodule external/RALibretro (with its own miniz/rcheevos submodules checked out);
# RALIBRETRO_SOURCE_DIR overrides it. Our shim and the build output stay in SCRIPT_DIR.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MAIN_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SUBMODULE_ROOT="$(cd "${RALIBRETRO_SOURCE_DIR:-$MAIN_ROOT/external/RALibretro}" && pwd)"
cd "$SUBMODULE_ROOT"

# Our changes to RALibretro's own sources, applied idempotently to the checkout (the same
# scheme as deps/mariadb-server). They show as modified inside external/RALibretro afterwards.
for p in "$SCRIPT_DIR"/patches/*.patch; do
  [[ -e "$p" ]] || continue
  if patch -p1 -R -f --dry-run <"$p" >/dev/null 2>&1; then
    echo "Patch already applied: $(basename "$p")"
  elif patch -p1 -N -f --dry-run <"$p" >/dev/null 2>&1; then
    patch -p1 -N -f <"$p"
    echo "Applied patch: $(basename "$p")"
  else
    echo "Error: patch $(basename "$p") does not apply to $SUBMODULE_ROOT -- refresh it" >&2
    exit 1
  fi
done

TARGET="auto"
TYPE="dynamic"
ARCH=""
# No upstream version to track, so each build is named after its build date.
VERSION="$(date +%Y%m%d)"

usage() {
  cat <<EOF
Usage: deps/RALibretro/build_rahasher.sh [--target auto|mac|windows|linux|iphone|android] [--arch x64|arm64] [--type dynamic|static|bin|all] [--version <name>]

Defaults:
  --target auto
  --arch   the host's for windows/linux; arm64 for mac/iphone/android
  --type dynamic
  --version <build date, YYYYMMdd>
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      TARGET="${2:-}"
      shift 2
      ;;
    --type)
      TYPE="${2:-}"
      shift 2
      ;;
    --arch)
      ARCH="${2:-}"
      shift 2
      ;;
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage
      exit 1
      ;;
  esac
done

UNAME_S="$(uname -s)"
case "$UNAME_S" in
  Darwin) HOST_PLATFORM="mac" ;;
  Linux) HOST_PLATFORM="linux" ;;
  MINGW*|MSYS*|CYGWIN*) HOST_PLATFORM="windows" ;;
  *) HOST_PLATFORM="unknown" ;;
esac

if [[ "$TARGET" == "auto" ]]; then
  if [[ "$HOST_PLATFORM" == "unknown" ]]; then
    echo "Cannot auto-detect target for uname=$UNAME_S" >&2
    exit 1
  fi
  TARGET="$HOST_PLATFORM"
fi

case "$(uname -m)" in
  arm64|aarch64) HOST_ARCH="arm64" ;;
  *) HOST_ARCH="x64" ;;
esac
case "$ARCH" in
  aarch64) ARCH="arm64" ;;
  x86_64|amd64) ARCH="x64" ;;
esac
case "$TARGET" in
  mac|iphone|android) ARCH="${ARCH:-arm64}" ;;
  windows|linux) ARCH="${ARCH:-$HOST_ARCH}" ;;
  *)
    echo "Unsupported target: $TARGET" >&2
    exit 1
    ;;
esac
case "$TARGET/$ARCH" in
  mac/arm64|iphone/arm64|android/arm64|windows/x64|windows/arm64|linux/x64|linux/arm64) ;;
  *)
    echo "Unsupported target/arch: $TARGET/$ARCH" >&2
    exit 1
    ;;
esac

# Cross compilers for the arm64 targets that are not native here. Passed on make's command
# line: Makefile.rahasher's `CC ?= gcc` never applies (make predefines CC), so an
# environment CC is the only other way in, and that leaks into every sub-make.
MAKE_TOOLS=()
if [[ "$TARGET/$ARCH" == "windows/arm64" ]]; then
  # MSYS2's MinGW64 gcc has no aarch64 target; llvm-mingw's clang does.
  CC="${CC_WINDOWS_ARM64:-aarch64-w64-mingw32-clang}"
  CXX="${CXX_WINDOWS_ARM64:-aarch64-w64-mingw32-clang++}"
  command -v "$CXX" >/dev/null 2>&1 || { echo "Error: $CXX not found. Put llvm-mingw's bin/ on PATH (https://github.com/mstorsjo/llvm-mingw)." >&2; exit 1; }
  MAKE_TOOLS=(CC="$CC" CXX="$CXX" AR="${AR_WINDOWS_ARM64:-llvm-ar}")
elif [[ "$TARGET/$ARCH" == "linux/arm64" && "$HOST_ARCH" != "arm64" ]]; then
  prefix="${LINUX_ARM64_CROSS_PREFIX:-aarch64-linux-gnu-}"
  command -v "${prefix}g++" >/dev/null 2>&1 || { echo "Error: ${prefix}g++ not found. Install g++-aarch64-linux-gnu." >&2; exit 1; }
  MAKE_TOOLS=(CC="${prefix}gcc" CXX="${prefix}g++" AR="${prefix}ar")
elif [[ "$TARGET" == "android" ]]; then
  # The NDK's clang for API 21, found the way chdman-simd's build finds it (chdlite's APK
  # loads this library). r26+, for the C++23 the sources use.
  : "${ANDROID_NDK_HOME:?ANDROID_NDK_HOME must point to an installed Android NDK (r26+)}"
  case "$(uname -s)" in
    Darwin) host_tag="darwin-x86_64" ;;
    Linux) host_tag="linux-x86_64" ;;
    MINGW*|MSYS*|CYGWIN*) host_tag="windows-x86_64" ;;
  esac
  ndk_bin="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$host_tag/bin"
  [[ -x "$ndk_bin/aarch64-linux-android21-clang++" || -f "$ndk_bin/aarch64-linux-android21-clang++.cmd" ]] || { echo "Error: NDK clang not found in $ndk_bin" >&2; exit 1; }
  MAKE_TOOLS=(CC="$ndk_bin/aarch64-linux-android21-clang" CXX="$ndk_bin/aarch64-linux-android21-clang++" AR="$ndk_bin/llvm-ar")
fi

case "$TYPE" in
  dynamic|static|bin)
    MAKE_TARGETS=("$TYPE")
    ;;
  all)
    MAKE_TARGETS=(dynamic static bin)
    ;;
  *)
    echo "Unsupported build type: $TYPE" >&2
    exit 1
    ;;
esac

if [[ "$TARGET" == "iphone" || "$TARGET" == "android" ]]; then
  echo "Note: cross-compilation requires external toolchain setup (CC/CXX/SDK) in your environment." >&2
elif [[ "$TARGET" == "windows" && "$HOST_PLATFORM" != "windows" ]]; then
  echo "Note: cross-compiling for windows from $HOST_PLATFORM requires a mingw-w64 CC/CXX toolchain in your environment." >&2
fi

if [[ "$TARGET" == "windows" && "$HOST_PLATFORM" == "windows" && "$ARCH" == "x64" ]]; then
  CXX_BIN="${CXX:-g++}"
  if ! command -v "$CXX_BIN" >/dev/null 2>&1; then
    echo "Error: no C++ compiler found ('$CXX_BIN')." >&2
    echo "Install the MSYS2 mingw-w64-x86_64-toolchain package and run this script from an MSYS2/MinGW (or Git Bash with mingw64 on PATH) shell." >&2
    exit 1
  fi
fi

for mt in "${MAKE_TARGETS[@]}"; do
  echo "Building $mt for target=$TARGET arch=$ARCH version=$VERSION"
  make -f "$SCRIPT_DIR/Makefile.rahasher" "$mt" U="$SCRIPT_DIR" TARGET_PLATFORM="$TARGET" TARGET_ARCH="$ARCH" VERSION="$VERSION" ${MAKE_TOOLS[@]+"${MAKE_TOOLS[@]}"}
done

OUT_ROOT="$SCRIPT_DIR/release/$TARGET/$ARCH/$VERSION"
echo "Build done. Output root: $OUT_ROOT"
echo "Dynamic lib: $OUT_ROOT/dynamic"
echo "Header: $OUT_ROOT/include"
