#!/usr/bin/env bash
# Downloads github.com/MetalSlug/MAMERedump's CHD/RVZ hash dats into
# samples/dats/mame-redump/: the top-level per-system dats (Redump-sourced discs,
# converted to CHD or, for GameCube/Wii, RVZ), MAME/ (softlist-based CHD hashes for
# systems only reachable through MAME itself, e.g. generic_cdrom), and Tosec/ (TOSEC's own
# catalog, also CHD-converted -- a 2020 snapshot, see scripts/fetch_tosec_latest.sh for the
# current/authoritative, non-CHD TOSEC source).
#
# Deliberately skipped: "Parent Clone" (same hashes as the top-level dats -- confirmed by
# diffing one -- just adds cloneof grouping and points at chdman-delta-compressed
# `.delta.chd` variants for local storage saving; irrelevant to a hash lookup) and
# "Missing" (an undumped-games wishlist, no hash data).
#
# Idempotent: a file already saved at the size GitHub currently reports for it is left
# alone; pass --force to refetch everything regardless.
#
# Portable to Linux, macOS and Windows (Git Bash/MSYS2/WSL). Needs `curl`, and either `jq`
# or `python3` to parse the GitHub API's directory listings.
#
# Usage: scripts/fetch_mame_redump_dats.sh [--force]

set -euo pipefail

FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BASE_OUT="$ROOT_DIR/samples/dats/mame-redump"

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }
if ! command -v jq >/dev/null 2>&1 && ! command -v python3 >/dev/null 2>&1; then
    echo "error: this script needs either jq or python3 to parse GitHub API responses" >&2
    exit 1
fi

REPO="MetalSlug/MAMERedump"

# Prints "name<TAB>size<TAB>download_url" per .dat file in $1 (a "MAME Redump[/sub]" path).
list_dats() {
    local api_path api_url listing
    api_path="$(printf '%s' "$1" | sed 's/ /%20/g')"
    api_url="https://api.github.com/repos/$REPO/contents/$api_path"
    listing="$(curl -fsSL -H "Accept: application/vnd.github+json" "$api_url")"
    if command -v jq >/dev/null 2>&1; then
        printf '%s' "$listing" | jq -r '.[] | select(.type=="file" and (.name | endswith(".dat"))) | [.name, .size, .download_url] | @tsv'
    else
        printf '%s' "$listing" | python3 -c '
import json, sys
for e in json.load(sys.stdin):
    if e["type"] == "file" and e["name"].endswith(".dat"):
        print(f"{e[\"name\"]}\t{e[\"size\"]}\t{e[\"download_url\"]}")
'
    fi
}

fetch_dir() {
    local api_path="$1" out_dir="$2"
    mkdir -p "$out_dir"
    echo "Listing $api_path..."
    local entries
    entries="$(list_dats "$api_path")"
    while IFS=$'\t' read -r name size url; do
        [ -z "$name" ] && continue
        local dest="$out_dir/$name"
        if [ "$FORCE" -eq 0 ] && [ -f "$dest" ] && [ "$(wc -c <"$dest" | tr -d ' ')" = "$size" ]; then
            echo "  up to date: $name"
            continue
        fi
        echo "  downloading: $name"
        curl -fsSL "$url" -o "$dest.part"
        mv "$dest.part" "$dest"
    done <<EOF
$entries
EOF
}

fetch_dir "MAME Redump" "$BASE_OUT"
fetch_dir "MAME Redump/MAME" "$BASE_OUT/MAME"
fetch_dir "MAME Redump/Tosec" "$BASE_OUT/Tosec"

echo "Done: $BASE_OUT ($(du -sh "$BASE_OUT" 2>/dev/null | cut -f1) total)"
