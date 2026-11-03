#!/usr/bin/env bash
set -euo pipefail

# Smoke checks for streaming hash integration scaffolding.
# Usage:
#   deps/7-Zip-zstd/tests/run-smoke.sh <platform> <arch> <version> [7zz_binary]
# Example:
#   deps/7-Zip-zstd/tests/run-smoke.sh mac arm64 2.3.90 ./CPP/7zip/Bundles/Alone2/_o/7zz

if [[ $# -lt 3 ]]; then
  echo "[ERROR] Usage: $0 <platform> <arch> <version> [7zz_binary]" >&2
  exit 2
fi

platform="$1"
arch="$2"
version="$3"
bin_path="${4:-}"

script_dir="$(cd "$(dirname "$0")" && pwd)"
user_dir="$(cd "$script_dir/.." && pwd)"
repo_root="$(cd "$user_dir/.." && pwd)"
log_dir="$script_dir/logs"
mkdir -p "$log_dir"
log_file="$log_dir/smoke-$(date +%Y%m%d-%H%M%S).log"

deps_base="$user_dir/deps/zlib-ng/$platform/$arch/$version"
include_dir="$deps_base/include"
dynamic_dir="$deps_base/dynamic"

{
  echo "[INFO] smoke start"
  echo "[INFO] deps_base=$deps_base"
} | tee -a "$log_file"

[[ -d "$include_dir" ]] || { echo "[ERROR] missing include: $include_dir" | tee -a "$log_file"; exit 3; }
[[ -d "$dynamic_dir" ]] || { echo "[ERROR] missing dynamic: $dynamic_dir" | tee -a "$log_file"; exit 3; }

if [[ "$platform" == "mac" ]]; then
  [[ -f "$dynamic_dir/libz.dylib" ]] || { echo "[ERROR] missing $dynamic_dir/libz.dylib" | tee -a "$log_file"; exit 4; }
fi

if [[ -n "$bin_path" ]]; then
  if [[ ! -x "$bin_path" ]]; then
    echo "[ERROR] binary is not executable: $bin_path" | tee -a "$log_file"
    exit 5
  fi

  echo "[INFO] checking runtime linkage" | tee -a "$log_file"
  case "$platform" in
    mac)
      otool -L "$bin_path" | tee -a "$log_file"
      ;;
    linux)
      ldd "$bin_path" | tee -a "$log_file"
      ;;
    windows)
      echo "[INFO] dumpbin check is not automated in this script" | tee -a "$log_file"
      ;;
  esac
fi

echo "[INFO] smoke done" | tee -a "$log_file"
