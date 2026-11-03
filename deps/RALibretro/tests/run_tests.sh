#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:-dev}"

d="$(cd "$(dirname "$0")/.." && pwd)"
cd "${RALIBRETRO_SOURCE_DIR:-$d/../../external/RALibretro}"
make -f "$d/Makefile.rahasher" test U="$d" TARGET_PLATFORM=mac TARGET_ARCH=arm64 VERSION="$VERSION"

echo "All tests passed"
