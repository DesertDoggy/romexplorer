#!/usr/bin/env bash
# Downloads No-Intro's daily "Love Pack" from datomatic.no-intro.org: every category
# (No-Intro, Source Code, Unofficial, Non-Redump, Non-Game, Redump Custom, Redump BIOS)
# and every archive variant (Standard, Numbered, Aftermarket) in one zip, which in turn
# holds one .dat per system *and variant* (e.g. a 3DS system gets separate Encrypted and
# Decrypted dats; other systems get their own headered/unheadered, byte-order, etc. splits
# the same way) -- so this one download already covers every format variant, no per-system
# selection needed.
#
# The site has no direct download link for this: it's a 3-step flow (GET the form to read
# its current dated submit-button name -> POST the selections, which 302s to a job/manager
# page -> POST that page's own (randomly-named, anti-scraping) submit button to get the
# actual file). This script does all three steps. The zip's own filename already carries
# the pack's date, so it isn't renamed.
#
# Portable to Linux, macOS and Windows (Git Bash/MSYS2/WSL). Needs `curl`.
#
# Usage: scripts/fetch_no_intro_daily.sh [--force]

set -euo pipefail

FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT_DIR="$ROOT_DIR/samples/dats/no-intro"
mkdir -p "$OUT_DIR"

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }

BASE="https://datomatic.no-intro.org"
FORM_URL="$BASE/index.php?page=download&op=daily&s=64"
UA="Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36"
JAR="$(mktemp)"
trap 'rm -f "$JAR"' EXIT

echo "Fetching $FORM_URL for the current submit-button name..."
form_html="$(curl -fsSL -b "$JAR" -c "$JAR" -A "$UA" "$FORM_URL")"
submit_name="$(printf '%s' "$form_html" | grep -oE 'name="daily_dl_[^"]+"' | head -1 | sed -E 's/name="([^"]+)"/\1/')"
if [ -z "$submit_name" ]; then
    echo "error: could not find the daily request form's submit button on $FORM_URL" >&2
    exit 1
fi

echo "Requesting every category and archive variant..."
prepare_headers="$(mktemp)"
curl -fsS -D "$prepare_headers" -b "$JAR" -c "$JAR" -A "$UA" -e "$FORM_URL" \
    -F "dat_type=dat" \
    -F "set[1]=Ok" -F "set[2]=Ok" -F "set[3]=Ok" -F "set[4]=Ok" -F "set[5]=Ok" \
    -F "set_redump_custom=Ok" \
    -F "set_redump_bios=Ok" \
    -F "include_standard=Ok" \
    -F "include_numbered=Ok" \
    -F "include_aftermarket=Ok" \
    -F "$submit_name=Request" \
    "$FORM_URL" -o /dev/null

manager_path="$(grep -i '^location:' "$prepare_headers" | tr -d '\r' | sed -E 's/^[Ll]ocation: *//')"
rm -f "$prepare_headers"
if [ -z "$manager_path" ]; then
    echo "error: the request didn't redirect to a job/manager page as expected" >&2
    exit 1
fi
manager_url="$BASE/$manager_path"

echo "Waiting on the job page ($manager_url) for the download button..."
manager_html="$(curl -fsSL -b "$JAR" -c "$JAR" -A "$UA" -e "$FORM_URL" "$manager_url")"
# Two submit buttons share the "Download!!" value: a `display:none` honeypot and the real,
# visible one. Only the line WITHOUT `display:none` is the one to click.
download_name="$(printf '%s' "$manager_html" | grep 'type="submit"' | grep -v 'display:none' | grep -oE 'name="[a-f0-9]+"' | head -1 | sed -E 's/name="([^"]+)"/\1/')"
if [ -z "$download_name" ]; then
    echo "error: could not find the manager page's download button" >&2
    exit 1
fi

echo "Downloading the pack..."
tmp_file="$(mktemp "$OUT_DIR/download.XXXXXX")"
final_headers="$(mktemp)"
curl -fsS -D "$final_headers" -b "$JAR" -c "$JAR" -A "$UA" -e "$manager_url" \
    -F "$download_name=Download!!" \
    "$manager_url" -o "$tmp_file"

filename="$(grep -i '^content-disposition:' "$final_headers" | tr -d '\r' | sed -E 's/.*filename="([^"]+)".*/\1/')"
rm -f "$final_headers"
if [ -z "$filename" ]; then
    echo "error: the download response had no filename" >&2
    rm -f "$tmp_file"
    exit 1
fi

if [ "$FORCE" -eq 0 ] && [ -f "$OUT_DIR/$filename" ]; then
    echo "Already up to date: $filename (unchanged since last run)"
    rm -f "$tmp_file"
    exit 0
fi

mv "$tmp_file" "$OUT_DIR/$filename"
echo "Saved $OUT_DIR/$filename"
