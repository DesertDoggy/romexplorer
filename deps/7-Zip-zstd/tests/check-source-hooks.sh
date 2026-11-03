#!/usr/bin/env bash
set -euo pipefail

# Source-level checks for stream-hash integration hooks.

script_dir="$(cd "$(dirname "$0")" && pwd)"
repo_root="${SEVENZIP_SOURCE_DIR:-$(cd "$script_dir/../../.." && pwd)/external/7-Zip-zstd}"
log_dir="$script_dir/logs"
mkdir -p "$log_dir"
log_file="$log_dir/source-hooks-$(date +%Y%m%d-%H%M%S).log"

cd "$repo_root"

checks=(
  "CPP/7zip/Archive/IArchive.h:IArchiveExtractCallbackData2"
  "CPP/7zip/Archive/IArchive.h:IArchiveExtractCallbackDataFile2"
  "CPP/7zip/Archive/IArchive.h:NDataAction"
  "CPP/7zip/Archive/7z/7zExtract.cpp:DataCallback"
  "CPP/7zip/Archive/7z/7zExtract.cpp:DataFileCallback"
  "CPP/7zip/Archive/7z/7zExtract.cpp:StopByDataCallback"
  "CPP/7zip/Archive/7z/7zExtract.cpp:OnFileBegin("
  "CPP/7zip/Archive/7z/7zExtract.cpp:OnFileEnd("
  "CPP/7zip/Archive/7z/7zExtract.cpp:OnData("
)

for item in "${checks[@]}"; do
  file="${item%%:*}"
  pattern="${item#*:}"
  if grep -Fq "$pattern" "$file"; then
    echo "[OK] $file contains: $pattern" | tee -a "$log_file"
  else
    echo "[ERROR] $file missing: $pattern" | tee -a "$log_file"
    exit 1
  fi
done

echo "[INFO] source hook checks passed" | tee -a "$log_file"
