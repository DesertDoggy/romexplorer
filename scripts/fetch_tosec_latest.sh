#!/usr/bin/env bash
# Downloads TOSEC's current "Complete" DAT pack (native per-format hashes -- crc32/md5/
# sha1 of the actual dumped .iso/.bin/.img/etc, not CHD-converted; see
# scripts/fetch_mame_redump_dats.sh's Tosec/ for a CHD-hashed but older snapshot) from
# tosecdev.org/downloads into samples/dats/tosec/.
#
# Scrapes the downloads page for whichever dated release category is newest (so a later
# rerun picks up a new TOSEC release automatically), then that category page for its
# "Complete" pack's own download link. The resulting zip's filename already carries the
# release date (e.g. "TOSEC - DAT Pack - Complete (4743) (TOSEC-v2025-03-13).zip"), so it
# isn't renamed.
#
# Idempotent: a file already saved under that exact name is left alone; pass --force to
# refetch regardless.
#
# Portable to Linux, macOS and Windows (Git Bash/MSYS2/WSL). Needs `curl`.
#
# Usage: scripts/fetch_tosec_latest.sh [--force]

set -euo pipefail

FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_DIR="$ROOT_DIR/samples/dats/tosec"
mkdir -p "$OUT_DIR"

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }

BASE="https://www.tosecdev.org"
UA="Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36"

echo "Finding the latest datfiles release on $BASE/downloads..."
downloads_html="$(curl -fsSL -A "$UA" "$BASE/downloads")"
# Every dated release category link looks like /downloads/category/<id>-<YYYY-MM-DD>;
# the highest date (not the highest id -- ids aren't issued in date order in the archive)
# is the current release.
latest_path="$(printf '%s' "$downloads_html" \
    | grep -oE 'href="/downloads/category/[0-9]+-[0-9]{4}-[0-9]{2}-[0-9]{2}"' \
    | sed -E 's/href="(.*)"/\1/' \
    | sed -E 's#.*/([0-9]+)-([0-9]{4}-[0-9]{2}-[0-9]{2})#\2 /downloads/category/\1-\2#' \
    | sort -r | head -1 | cut -d' ' -f2-)"
if [ -z "$latest_path" ]; then
    echo "error: could not find a dated release category on $BASE/downloads" >&2
    exit 1
fi
echo "Latest release: $latest_path"

category_html="$(curl -fsSL -A "$UA" "$BASE$latest_path")"
download_href="$(printf '%s' "$category_html" | grep -oE 'href="[^"]*\?download=[0-9]+:[^"]*complete[^"]*"' -i | head -1 | sed -E 's/href="([^"]+)"/\1/')"
if [ -z "$download_href" ]; then
    echo "error: could not find the \"Complete\" pack's download link on $BASE$latest_path" >&2
    exit 1
fi
# The href is already absolute-path or relative to $latest_path; normalize to absolute.
case "$download_href" in
    http*) download_url="$download_href" ;;
    /*) download_url="$BASE$download_href" ;;
    *) download_url="$BASE$latest_path$download_href" ;;
esac

echo "Downloading $download_url"
tmp_file="$(mktemp "$OUT_DIR/download.XXXXXX")"
headers="$(mktemp)"
curl -fsSL -A "$UA" -D "$headers" "$download_url" -o "$tmp_file"
filename="$(grep -i '^content-disposition:' "$headers" | tr -d '\r' | sed -E 's/.*filename="([^"]+)".*/\1/')"
rm -f "$headers"
if [ -z "$filename" ]; then
    echo "error: the download response had no filename" >&2
    rm -f "$tmp_file"
    exit 1
fi

if [ "$FORCE" -eq 0 ] && [ -f "$OUT_DIR/$filename" ]; then
    echo "Already up to date: $filename"
    rm -f "$tmp_file"
    exit 0
fi

mv "$tmp_file" "$OUT_DIR/$filename"
echo "Saved $OUT_DIR/$filename"
