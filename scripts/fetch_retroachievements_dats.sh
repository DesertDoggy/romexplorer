#!/usr/bin/env bash
# Downloads every RetroAchievements DAT (one per system, "No Subfolders" variant --
# structured title/region/cloneof fields, not RomVault's folder-path naming; see this
# session's plan file for why) from github.com/UltraGodAzgorath/UGA_RA_DATs into
# samples/dats/retroachievements/, named exactly as upstream (these don't carry a date
# in the name the way the other three sources' files do).
#
# Idempotent: a file already saved at the size GitHub currently reports for it is left
# alone; pass --force to refetch everything regardless.
#
# Portable to Linux, macOS and Windows (Git Bash/MSYS2/WSL). Needs `curl`, and either `jq`
# or `python3` to parse the GitHub API's directory listing (falls back to `python3` if `jq`
# isn't installed).
#
# Usage: scripts/fetch_retroachievements_dats.sh [--force]

set -euo pipefail

FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_DIR="$ROOT_DIR/samples/dats/retroachievements"
mkdir -p "$OUT_DIR"

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }

REPO="UltraGodAzgorath/UGA_RA_DATs"
DIR="DATs/RetroAchievements (No Subfolders)"
API_URL="https://api.github.com/repos/$REPO/contents/$(printf '%s' "$DIR" | sed 's/ /%20/g')"

echo "Listing $DIR in $REPO..."
listing="$(curl -fsSL -H "Accept: application/vnd.github+json" "$API_URL")"

# Prints "name<TAB>size<TAB>download_url" per .dat file, one per line.
if command -v jq >/dev/null 2>&1; then
    entries="$(printf '%s' "$listing" | jq -r '.[] | select(.name | endswith(".dat")) | [.name, .size, .download_url] | @tsv')"
elif command -v python3 >/dev/null 2>&1; then
    entries="$(printf '%s' "$listing" | python3 -c '
import json, sys
for e in json.load(sys.stdin):
    if e["name"].endswith(".dat"):
        print(f"{e[\"name\"]}\t{e[\"size\"]}\t{e[\"download_url\"]}")
')"
else
    echo "error: this script needs either jq or python3 to parse the GitHub API response" >&2
    exit 1
fi

if [ -z "$entries" ]; then
    echo "error: no .dat files found in the listing" >&2
    exit 1
fi

count=0
while IFS=$'\t' read -r name size url; do
    [ -z "$name" ] && continue
    dest="$OUT_DIR/$name"
    if [ "$FORCE" -eq 0 ] && [ -f "$dest" ] && [ "$(wc -c <"$dest" | tr -d ' ')" = "$size" ]; then
        echo "  up to date: $name"
        continue
    fi
    echo "  downloading: $name"
    curl -fsSL "$url" -o "$dest.part"
    mv "$dest.part" "$dest"
    count=$((count + 1))
done <<EOF
$entries
EOF

echo "Done: $OUT_DIR ($count file(s) fetched, $(du -sh "$OUT_DIR" 2>/dev/null | cut -f1) total)"
