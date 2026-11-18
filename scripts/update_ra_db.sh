#!/usr/bin/env bash
# Downloads RetroAchievements' hash list and writes radb.bin: every game that has achievements,
# its console, its achievement count, the RA hashes it supports and its genre. chdlite's Hash
# looks a file's RA hash up in it (chdlite/src/ra.rs, which documents the format); the date it
# was made (yyyyMMdd) is the list's version, shown in chdlite's settings and `chdlite --version`.
#
# Run it again to update the list, then rebuild chdlite (build.rs compiles the file in), or
# put the new radb.bin beside an installed chdlite (in its folder or its assets/), which wins
# over the compiled-in one.
#
# Genres come from one request per game, so the first run takes hours (a second apart, about
# one request per game with achievements). They are kept in a cache file and later runs only
# ask for games not in it. Stopping a run loses nothing: the next run picks up where it was.
#
# Needs a RetroAchievements web API key (retroachievements.org > Settings > Keys): RA_API_KEY
# in the environment, else `ra_api_key` in the --key-file (default app_root_ex/assets/api.env,
# romexplorer's own). Needs curl and Python 3.
#
# Usage: scripts/update_ra_db.sh [options]
#   --out <file>          radb.bin to write (default app_root_ex/assets/radb.bin)
#   --work <dir>          downloaded JSON (default .test-radb/ in the repo, removed when done)
#   --key-file <file>     api.env holding ra_api_key (default app_root_ex/assets/api.env)
#   --genre-cache <file>  genre cache, game id -> genre (default .test-radb-cache/genres.json)
#   --no-genres           skip genres: no per-game requests, no genre section
#   --refresh-genres      ask again for every game's genre, not only new games
#   --keep-json           keep the --work folder
#
# radb.bin version 1 with a genre section appended after the hash list, which readers that do
# not know it ignore:
#   4  magic b"GENR";  2  genre_count u16 LE;  genre_count x (2 len u16 LE + UTF-8 name);
#   game_count x 2  genre index u16 LE per game, in game order (0xFFFF = none)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUT="$ROOT_DIR/app_root_ex/assets/radb.bin"
WORK="$ROOT_DIR/.test-radb"
KEY_FILE="$ROOT_DIR/app_root_ex/assets/api.env"
GENRE_CACHE="$ROOT_DIR/.test-radb-cache/genres.json"
GENRES=1 REFRESH=0 KEEP=0
need() { [[ -n "${2:-}" ]] || { echo "error: $1 needs a path" >&2; exit 2; }; }
while [[ $# -gt 0 ]]; do
    case "$1" in
        --out) need "$1" "${2:-}"; OUT="$2"; shift ;;
        --work) need "$1" "${2:-}"; WORK="$2"; shift ;;
        --key-file) need "$1" "${2:-}"; KEY_FILE="$2"; shift ;;
        --genre-cache) need "$1" "${2:-}"; GENRE_CACHE="$2"; shift ;;
        --no-genres) GENRES=0 ;;
        --refresh-genres) REFRESH=1 ;;
        --keep-json) KEEP=1 ;;
        -h|--help) sed -n '2,33p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
if [[ -z "$KEY" && -f "$KEY_FILE" ]]; then
    KEY="$(sed -n 's/^[[:space:]]*\(export[[:space:]]\{1,\}\)\{0,1\}ra_api_key[[:space:]]*=[[:space:]]*//p' "$KEY_FILE" | tail -1 | tr -d '\r' | sed 's/^["'\'']//; s/["'\'']$//')"
fi
[[ -n "$KEY" ]] || { echo "error: no RetroAchievements web API key: set RA_API_KEY, or ra_api_key in $KEY_FILE" >&2; exit 1; }

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

# Genres: merge what an earlier, stopped run left in $WORK into the cache, list the games with
# achievements the cache does not have (all of them with --refresh-genres), ask for each, and
# merge again. The cache is written every 100 games, so stopping costs at most that many.
merge_genres() {
    "$PY" - "$WORK" "$GENRE_CACHE" <<'PY'
import glob, json, os, sys
work, cache = sys.argv[1], sys.argv[2]
known = json.load(open(cache, encoding="utf-8")) if os.path.isfile(cache) else {}
for path in glob.glob(os.path.join(work, "game_*.json")):
    try:
        game = json.load(open(path, encoding="utf-8"))
    except (OSError, ValueError):
        os.remove(path)  # not recorded, so the next run asks again
        continue
    gid = os.path.basename(path)[5:-5]
    known[gid] = (game.get("Genre") or "").strip() if isinstance(game, dict) else ""
    os.remove(path)
os.makedirs(os.path.dirname(cache) or ".", exist_ok=True)
with open(cache + ".tmp", "w", encoding="utf-8") as f:
    json.dump(known, f, ensure_ascii=False, sort_keys=True, indent=0)
os.replace(cache + ".tmp", cache)
PY
}

if [[ "$GENRES" -eq 1 ]]; then
    merge_genres
    wanted="$("$PY" - "$WORK" "$GENRE_CACHE" "$REFRESH" <<'PY'
import glob, json, os, sys
work, cache, refresh = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
known = {} if refresh else json.load(open(cache, encoding="utf-8"))
ids = set()
for path in glob.glob(os.path.join(work, "games_*.json")):
    for game in json.load(open(path, encoding="utf-8")):
        if int(game.get("NumAchievements") or 0) > 0 and game.get("Hashes"):
            ids.add(str(game["ID"]))
print(" ".join(sorted(ids - known.keys(), key=int)))
PY
)"
    total=$(wc -w <<<"$wanted") count=0
    echo "Genres: $total games to ask for (the rest are in $GENRE_CACHE)"
    for id in $wanted; do
        count=$((count + 1))
        printf '[%d/%d] game %s genre\n' "$count" "$total" "$id"
        fetch API_GetGame "i=$id" "$WORK/game_$id.json" || echo "warning: game $id's genre did not download; the next run asks again" >&2
        (( count % 100 == 0 )) && merge_genres
        sleep 1
    done
    merge_genres
fi

"$PY" - "$WORK" "$OUT" "$GENRES" "$GENRE_CACHE" <<'PY'
import glob, json, os, struct, sys, time

work, out, with_genres, cache = sys.argv[1], sys.argv[2], sys.argv[3] == "1", sys.argv[4]
known = json.load(open(cache, encoding="utf-8")) if with_genres and os.path.isfile(cache) else {}
games, hashes, game_genres = [], [], []
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
        game_genres.append(known.get(str(game["ID"])) or "")
hashes.sort()
if not games:
    sys.exit("error: no games with achievements in the downloaded lists")
date = time.strftime("%Y%m%d").encode("ascii")
body = b"RADB" + bytes([1]) + date + struct.pack("<I", len(games)) + b"".join(games)
body += struct.pack("<I", len(hashes)) + b"".join(h + struct.pack("<I", i) for h, i in hashes)
genre_bytes, named = 0, 0
if with_genres:
    names = sorted({g for g in game_genres if g})[:0xFFFE]
    slot = {g: i for i, g in enumerate(names)}
    section = b"GENR" + struct.pack("<H", len(names))
    for g in names:
        raw = g.encode("utf-8")[:65535]
        section += struct.pack("<H", len(raw)) + raw
    section += b"".join(struct.pack("<H", slot.get(g, 0xFFFF)) for g in game_genres)
    named = sum(1 for g in game_genres if g in slot)
    genre_bytes = len(section)
    body += section
os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
tmp = out + ".tmp"
with open(tmp, "wb") as f:
    f.write(body)
os.replace(tmp, out)
print(f"wrote {out}: version {date.decode()}, {len(games)} games, {len(hashes)} hashes, {len(body)} bytes")
if with_genres:
    print(f"genres: {named} of {len(games)} games have one, {len(set(g for g in game_genres if g))} different, genre section {genre_bytes} bytes")
PY

[[ "$KEEP" -eq 1 ]] || rm -rf "$WORK"
