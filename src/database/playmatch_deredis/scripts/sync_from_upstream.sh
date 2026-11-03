#!/bin/sh
# Refreshes src/providers/*.rs from external/playmatch's current checkout, then reapplies
# this directory's patches on top. Run this whenever the playmatch submodule is bumped to a
# newer commit -- day-to-day `cargo build` never touches this script or applies a patch;
# the vendored, already-patched .rs files under src/ are what gets compiled, same as any
# other committed source.
#
# Loud failure by design: if playmatch's upstream has changed a file a patch here targets,
# `patch`'s own dry-run reports it and this script stops before overwriting anything, so a
# stale-but-working vendored copy is never silently replaced with a broken one. See
# ../README.md for the two-mechanism explanation (plain-copy files vs patched files) and
# deps/mariadb-server/scripts/build_mariadb_server.sh's apply_patches() for the
# same dry-run/apply/loud-failure pattern used here.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
ROOT_DIR=$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)
UPSTREAM="${ROOT_DIR}/../../../external/playmatch/service/src"
DST="${ROOT_DIR}/src"
PATCH_DIR="${ROOT_DIR}/patches"

if [ ! -f "${UPSTREAM}/providers/igdb/mod.rs" ]; then
    echo "[ERROR] Upstream playmatch source not found at: ${UPSTREAM}" >&2
    echo "[ERROR] Expected the 'playmatch' submodule at external/playmatch (checked out)." >&2
    exit 2
fi

log() { printf '%s\n' "$*"; }

# Providers kept as live-API clients. launchbox/openvgdb/retroachievements are deliberately
# never synced here: tracing their actual code showed their only real operation is
# ensure_imported() -- triggering a bulk snapshot download straight into playmatch's own
# Postgres tables, not a live per-item API call -- so there is nothing here worth vendoring
# for them. If a future playmatch version adds a genuine live lookup to one of them, add it
# here and give it the same patch treatment as the others, following this file's pattern.
PROVIDERS="igdb screenscraper mobygames steamgriddb emuready hasheous thegamesdb"

log "[INFO] Copying unchanged shared files..."
cp "${UPSTREAM}/config/mod.rs" "${DST}/config/mod.rs"
# providers/mod.rs's own file-level doc comment already covers why config/mod.rs's
# `pub mod versions;` line is stripped after this copy (that submodule was never vendored).
sed -i '/^pub mod versions;$/d' "${DST}/config/mod.rs"
cp "${UPSTREAM}/config/http.rs" "${DST}/config/http.rs"
cp "${UPSTREAM}/http/abstraction.rs" "${DST}/http/abstraction.rs"

log "[INFO] Copying fresh provider files (pre-patch)..."
mkdir -p "${DST}/providers/igdb"
cp "${UPSTREAM}/providers/igdb/mod.rs" "${DST}/providers/igdb/mod.rs"
cp "${UPSTREAM}/providers/igdb/model.rs" "${DST}/providers/igdb/model.rs"
cp "${UPSTREAM}/providers/igdb/apicalypse.rs" "${DST}/providers/igdb/apicalypse.rs"
cp "${UPSTREAM}/providers/igdb/constants.rs" "${DST}/providers/igdb/constants.rs"
for p in mobygames steamgriddb emuready hasheous; do
    mkdir -p "${DST}/providers/${p}"
    cp "${UPSTREAM}/providers/${p}/mod.rs" "${DST}/providers/${p}/mod.rs"
    cp "${UPSTREAM}/providers/${p}/model.rs" "${DST}/providers/${p}/model.rs"
done
mkdir -p "${DST}/providers/screenscraper"
cp "${UPSTREAM}/providers/screenscraper/mod.rs" "${DST}/providers/screenscraper/mod.rs"
cp "${UPSTREAM}/providers/screenscraper/model.rs" "${DST}/providers/screenscraper/model.rs"
mkdir -p "${DST}/providers/thegamesdb"
cp "${UPSTREAM}/providers/thegamesdb/mod.rs" "${DST}/providers/thegamesdb/mod.rs"
cp "${UPSTREAM}/providers/thegamesdb/api.rs" "${DST}/providers/thegamesdb/api.rs"
cp "${UPSTREAM}/providers/thegamesdb/model.rs" "${DST}/providers/thegamesdb/model.rs"

log "[INFO] Applying patches..."
cd "${DST}"
failures=""
for patch_file in "${PATCH_DIR}"/*.patch; do
    [ -e "${patch_file}" ] || continue
    name=$(basename "${patch_file}")
    if patch -p1 --dry-run < "${patch_file}" >/dev/null 2>&1; then
        patch -p1 < "${patch_file}"
        log "    [OK] ${name}"
    else
        log "    [FAIL] ${name} -- does not apply. playmatch's upstream has likely changed"
        log "    [FAIL] the file(s) this patch targets. The pre-patch copy above is now"
        log "    [FAIL] sitting unpatched under src/ -- do not build until this is resolved:"
        log "    [FAIL] diff the new upstream file against patches/${name} by hand, update"
        log "    [FAIL] the patch (see ../../external/mariadb-server's equivalent script"
        log "    [FAIL] for the same regenerate-a-patch workflow), and re-run this script."
        failures="${failures} ${name}"
    fi
done

if [ -n "${failures}" ]; then
    echo "[ERROR] Sync completed with failed patches:${failures}" >&2
    exit 1
fi

log "[INFO] Sync complete. Run 'cargo build' (or 'cargo test') from ${ROOT_DIR} to verify."
