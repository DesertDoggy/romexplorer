#!/usr/bin/env bash
set -euo pipefail

# Run a command with zlib-ng dynamic library from user/deps.
# Usage:
#   user/scripts/run-with-user-zlib-ng.sh <platform> <arch> <version> -- <command> [args...]
# Example:
#   user/scripts/run-with-user-zlib-ng.sh mac arm64 2.3.90 -- ./7zz t sample.7z

if [[ $# -lt 5 ]]; then
  echo "[ERROR] Usage: $0 <platform> <arch> <version> -- <command> [args...]" >&2
  exit 2
fi

platform="$1"
arch="$2"
version="$3"
shift 3

if [[ "$1" != "--" ]]; then
  echo "[ERROR] Missing -- before command" >&2
  exit 2
fi
shift

if [[ $# -eq 0 ]]; then
  echo "[ERROR] Missing command after --" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "$0")" && pwd)"
user_dir="$(cd "$script_dir/.." && pwd)"
deps_base="$user_dir/deps/zlib-ng/$platform/$arch/$version"
include_dir="$deps_base/include"
dynamic_dir="$deps_base/dynamic"
log_dir="$user_dir/logs"
mkdir -p "$log_dir"
log_file="$log_dir/run-$(date +%Y%m%d-%H%M%S).log"

if [[ ! -d "$include_dir" ]]; then
  echo "[ERROR] Missing include dir: $include_dir" >&2
  exit 3
fi
if [[ ! -d "$dynamic_dir" ]]; then
  echo "[ERROR] Missing dynamic dir: $dynamic_dir" >&2
  exit 3
fi

case "$platform" in
  mac)
    lib_file="$dynamic_dir/libz.dylib"
    if [[ ! -f "$lib_file" ]]; then
      echo "[ERROR] Missing dynamic library: $lib_file" >&2
      exit 4
    fi
    export DYLD_LIBRARY_PATH="$dynamic_dir${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
    ;;
  linux)
    if ! ls "$dynamic_dir"/libz.so* >/dev/null 2>&1; then
      echo "[ERROR] Missing dynamic library: $dynamic_dir/libz.so*" >&2
      exit 4
    fi
    export LD_LIBRARY_PATH="$dynamic_dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    ;;
  windows)
    if ! ls "$dynamic_dir"/*.dll >/dev/null 2>&1; then
      echo "[ERROR] Missing dynamic library: $dynamic_dir/*.dll" >&2
      exit 4
    fi
    ;;
  *)
    echo "[ERROR] Unsupported platform: $platform" >&2
    exit 2
    ;;
esac

{
  echo "[INFO] platform=$platform arch=$arch version=$version"
  echo "[INFO] include_dir=$include_dir"
  echo "[INFO] dynamic_dir=$dynamic_dir"
  echo "[INFO] command=$*"
} | tee -a "$log_file"

"$@" 2>&1 | tee -a "$log_file"
