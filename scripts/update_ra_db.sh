#!/usr/bin/env bash
# Downloads RetroAchievements' hash list and writes app_root_ex/assets/radb.bin: every game
# that has achievements, its console, its achievement count and the RA hashes it supports.
# chdlite's Hash looks a file's RA hash up in it (chdlite/src/ra.rs, which documents the
# format); the date it was made (yyyyMMdd) is the list's version, shown in chdlite's settings
# and `chdlite --version`.
#
# Run it again to update the list, then rebuild chdlite (build.rs compiles the file in), or
# put the new radb.bin beside an installed chdlite (in its folder or its assets/), which wins
# over the compiled-in one.
#
# Needs a RetroAchievements web API key (retroachievements.org > Settings > Keys): RA_API_KEY
# in the environment, else `ra_api_key` in app_root_ex/assets/api.env (romexplorer's own).
# About 80 requests, a second apart. Needs curl and Python 3. The downloaded JSON goes to
# .test-radb/ in the repo (ignored by git), and is removed when the file is written.
#
# Usage: scripts/update_ra_db.sh [--out <file>] [--keep-json]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT="$ROOT_DIR/app_root_ex/assets/radb.bin"
WORK="$ROOT_DIR/.test-radb"
KEEP=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --out) OUT="$2"; shift ;;
        --keep-json) KEEP=1 ;;
        -h|--help) sed -n '2,18p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "error: unknown option '$1' (try --help)" >&2; exit 2 ;;
    esac
    shift
done

command -v curl >/dev/null 2>&1 || { echo "error: curl is required" >&2; exit 1; }
PY=""
for candidate in python3 python py; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -c 'import sys; sys.exit(sys.version_info < (3, 6))' 2>/dev/null; then
        PY="$candidate"; break
    fi
done
[[ -n "$PY" ]] || { echo "error: Python 3 is required" >&2; exit 1; }

KEY="${RA_API_KEY:-}"
if [[ -z "$KEY" && -f "$ROOT_DIR/app_root_ex/assets/api.env" ]]; then
    KEY="$(sed -n 's/^[[:space:]]*\(export[[:space:]]\{1,\}\)\{0,1\}ra_api_key[[:space:]]*=[[:space:]]*//p' "$ROOT_DIR/app_root_ex/assets/api.env" | tail -1 | tr -d '\r' | sed 's/^["'\'']//; s/["'\'']$//')"
fi
[[ -n "$KEY" ]] || { echo "error: no RetroAchievements web API key: set RA_API_KEY, or ra_api_key in app_root_ex/assets/api.env" >&2; exit 1; }

API="https://retroachievements.org/API"
mkdir -p "$WORK"
# The key goes to curl on stdin (a config line), never on its command line.
fetch() {
    local endpoint="$1" query="$2" dest="$3" try
    for try in 1 2 3; do
        if printf 'url = "%s/%s.php?%s&y=%s"\n' "$API" "$endpoint" "$query" "$KEY" \
            | curl -fsS --retry 2 -A "romexplorer-update-ra-db" -K - -o "$dest"; then
            return 0
        fi
        sleep $((try * 3))
    done
    return 1
}

echo "Consoles..."
fetch API_GetConsoleIDs "a=1&g=1" "$WORK/consoles.json"
ids="$("$PY" -c 'import json,sys; print(" ".join(str(c["ID"]) for c in json.load(open(sys.argv[1], encoding="utf-8"))))' "$WORK/consoles.json")"
[[ -n "$ids" ]] || { echo "error: RetroAchievements listed no consoles" >&2; exit 1; }

count=0 total=$(wc -w <<<"$ids")
for id in $ids; do
    count=$((count + 1))
    printf '[%d/%d] console %s\n' "$count" "$total" "$id"
    fetch API_GetGameList "i=$id&h=1&f=1" "$WORK/games_$id.json" || { echo "error: console $id's game list did not download" >&2; exit 1; }
    sleep 1
done

"$PY" - "$WORK" "$OUT" <<'PY'
import glob, json, os, struct, sys, time

work, out = sys.argv[1], sys.argv[2]
games, hashes = [], []
for path in sorted(glob.glob(os.path.join(work, "games_*.json"))):
    for game in json.load(open(path, encoding="utf-8")):
        achievements = int(game.get("NumAchievements") or 0)
        found = sorted({h.strip().lower() for h in (game.get("Hashes") or []) if len(h.strip()) == 32})
        if achievements <= 0 or not found:
            continue
        index = len(games)
        title = (game.get("Title") or "").encode("utf-8")[:65535]
        games.append(struct.pack("<IHHH", int(game["ID"]), int(game["ConsoleID"]), min(achievements, 65535), len(title)) + title)
        hashes.extend((bytes.fromhex(h), index) for h in found)
hashes.sort()
if not games:
    sys.exit("error: no games with achievements in the downloaded lists")
date = time.strftime("%Y%m%d").encode("ascii")
body = b"RADB" + bytes([1]) + date + struct.pack("<I", len(games)) + b"".join(games)
body += struct.pack("<I", len(hashes)) + b"".join(h + struct.pack("<I", i) for h, i in hashes)
os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
tmp = out + ".tmp"
with open(tmp, "wb") as f:
    f.write(body)
os.replace(tmp, out)
print(f"wrote {out}: version {date.decode()}, {len(games)} games, {len(hashes)} hashes, {len(body)} bytes")
PY

[[ "$KEEP" -eq 1 ]] || rm -rf "$WORK"
