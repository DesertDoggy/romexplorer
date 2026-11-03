#!/usr/bin/env bash
# Downloads github.com/Abdess/retrobios's `database.json` (BIOS/firmware hash metadata --
# name/size/crc32/md5/sha1/sha256, no binaries; see that repo's LICENSE/NOTICE for what the
# MIT license there does and doesn't cover) into samples/dats/retrobios/, named with the
# file's own last-commit time so successive runs keep a dated history instead of overwriting.
#
# Portable to Linux, macOS and Windows (Git Bash/MSYS2/WSL -- run it from one of those on
# Windows, not cmd.exe/PowerShell directly). Needs `curl` (bundled with Git Bash and with
# Windows 10+ itself) and, ideally, `jq` (falls back to a plain grep/sed parse of the GitHub
# API response if `jq` isn't installed).
#
# Usage: scripts/fetch_retrobios_database.sh

set -euo pipefail

REPO="Abdess/retrobios"
FILE_PATH="database.json"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_DIR="$ROOT_DIR/samples/dats/retrobios"

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }

echo "Looking up the latest commit that touched $FILE_PATH in $REPO..."
API_URL="https://api.github.com/repos/$REPO/commits?path=$FILE_PATH&per_page=1"
commit_json="$(curl -fsSL -H "Accept: application/vnd.github+json" "$API_URL")"

if command -v jq >/dev/null 2>&1; then
    commit_sha="$(printf '%s' "$commit_json" | jq -r '.[0].sha')"
    commit_date="$(printf '%s' "$commit_json" | jq -r '.[0].commit.committer.date')"
else
    # No jq: pull the first "sha"/"date" fields out with grep/sed. Good enough for the
    # GitHub API's pretty-printed JSON (one field per line); not a general JSON parser.
    commit_sha="$(printf '%s' "$commit_json" | grep -m1 '"sha"' | sed -E 's/.*"sha": *"([^"]+)".*/\1/')"
    commit_date="$(printf '%s' "$commit_json" | grep -m1 '"date"' | sed -E 's/.*"date": *"([^"]+)".*/\1/')"
fi

if [ -z "${commit_sha:-}" ] || [ -z "${commit_date:-}" ]; then
    echo "error: could not read the commit sha/date from the GitHub API response" >&2
    echo "$commit_json" >&2
    exit 1
fi

# "2024-05-01T12:34:56Z" -> "20240501123456" (YYYYMMDDhhmmss, UTC, the commit's own time).
commit_time="$(printf '%s' "$commit_date" | tr -d -- '-:TZ')"
out_file="$OUT_DIR/database_${commit_time}.json"

if [ -f "$out_file" ]; then
    echo "Already up to date: $out_file (commit $commit_sha, $commit_date) -- nothing to download."
    exit 0
fi

mkdir -p "$OUT_DIR"

# Fetched at the exact commit the timestamp names, not whatever `main` has moved to since.
raw_url="https://raw.githubusercontent.com/$REPO/$commit_sha/$FILE_PATH"
echo "Downloading $raw_url"
tmp_file="$out_file.part"
curl -fsSL "$raw_url" -o "$tmp_file"
mv "$tmp_file" "$out_file"

echo "Saved $out_file (commit $commit_sha, $commit_date)"
