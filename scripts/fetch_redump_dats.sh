#!/usr/bin/env bash
# Downloads every "Dat + Serial/Version" datfile (one zip per system, disc dumps with
# serial/version metadata) and every BIOS dat listed on https://redump.info/downloads into
# samples/dats/redump/. Both kinds of link already carry Redump's own "as of" date in the
# filename -- a datfile zip's via its `Content-Disposition` response header, a BIOS dat's
# via its URL -- so files are saved under that exact name, no renaming needed.
#
# Idempotent: a system/BIOS file already saved under its current (unchanged) name is left
# alone, so reruns only fetch what Redump has actually regenerated since the last run. Pass
# --force to refetch everything regardless.
#
# Portable to Linux, macOS and Windows (Git Bash/MSYS2/WSL -- run it from one of those on
# Windows, not cmd.exe/PowerShell directly). Needs `curl`.
#
# Usage: scripts/fetch_redump_dats.sh [--force]

set -euo pipefail

FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_DIR="$ROOT_DIR/samples/dats/redump"
mkdir -p "$OUT_DIR"

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }

DOWNLOADS_URL="https://redump.info/downloads"
echo "Fetching $DOWNLOADS_URL for the current system/BIOS link list..."
page="$(curl -fsSL "$DOWNLOADS_URL")"

# Every system that offers a "Dat + Serial/Version" datfile, exact case as linked.
codes="$(printf '%s' "$page" | grep -oE 'href="/datfile/[^"]+/serial,version"' | sed -E 's#href="/datfile/([^"]+)/serial,version"#\1#' | sort -u)"

# The BIOS section's direct .dat links (absolute paths, URL-encoded filenames).
bios_paths="$(printf '%s' "$page" | grep -oE 'href="/static/bios/[^"]+\.dat"' | sed -E 's/href="(.*)"/\1/' | sort -u)"

# Portable %XX decoder (no python/perl needed): turns "%20" into the byte 0x20 via printf's
# own \xHH support.
urldecode() {
    local s="${1//+/ }"
    printf '%b' "${s//%/\\x}"
}

download() {
    local url="$1" name="$2"
    if [ "$FORCE" -eq 0 ] && [ -f "$OUT_DIR/$name" ]; then
        echo "  up to date: $name"
        return
    fi
    echo "  downloading: $name"
    curl -fsSL "$url" -o "$OUT_DIR/$name.part"
    mv "$OUT_DIR/$name.part" "$OUT_DIR/$name"
    sleep 0.3 # a little courtesy delay between requests
}

system_count="$(printf '%s\n' "$codes" | grep -c . || true)"
echo "Disc datfiles (Dat + Serial/Version), $system_count systems:"
while IFS= read -r code; do
    [ -z "$code" ] && continue
    url="https://redump.info/datfile/$code/serial,version"
    filename="$(curl -fsSIL "$url" | tr -d '\r' | grep -i '^content-disposition:' | sed -E 's/.*filename="([^"]+)".*/\1/')"
    if [ -z "$filename" ]; then
        echo "  skip $code: no filename in the response"
        continue
    fi
    download "$url" "$filename"
done <<EOF
$codes
EOF

echo "BIOS datfiles:"
while IFS= read -r path; do
    [ -z "$path" ] && continue
    url="https://redump.info$path"
    filename="$(urldecode "${path##*/}")"
    download "$url" "$filename"
done <<EOF
$bios_paths
EOF

echo "Done: $OUT_DIR ($(du -sh "$OUT_DIR" 2>/dev/null | cut -f1))"
