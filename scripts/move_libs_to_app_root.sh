#!/usr/bin/env bash
# Moves every dependency's built native shared library into app_root_ex/lib/<os>/, the
# flat, deployed-app-shaped layout `native_lib::find_app_root_library` (and the mariadb
# support code) actually looks in -- as opposed to each dependency's own
# `deps/<name>/release/<platform>/<arch>/<version>/...` tree, which stays the dev-build source
# of truth and is still searched as a fallback by every wrapper if this script hasn't
# been run yet.
#
# Single-lib submodules (chdman-simd, dolphin, 7-Zip-zstd, zlib-ng, RALibretro) land
# flat: app_root_ex/lib/<os>/<arch>/<filename>. mariadb-server + mariadb-connector-c + openssl
# combine into one app_root_ex/lib/<os>/<arch>/mariadb/ tree (bin/shared/share) -- that's the
# only group actually consumed together (the embedded MariaDB server needs the connector
# client lib and OpenSSL's TLS libs alongside its own binaries), matching what has been
# assembled by hand there until now. libchdr/mame/playmatch produce no shared library at
# all and are skipped entirely.
#
# Only ever touches app_root_ex/lib/<os>/<arch>/ (git-ignored build output, not source) --
# never deps/ or the external/ submodules themselves. Safe to re-run: each copy overwrites cleanly,
# nothing is deleted first.
#
# A submodule not yet built for this platform/arch is warned about and skipped, not a
# hard failure -- a fresh checkout with only some native libs built should still work
# for the ones that are.
#
# Usage: scripts/move_libs_to_app_root.sh [--nostreaming]
#   Pulls chdman-simd/dolphin/7-Zip-zstd from their with_streaming/ builds (what their
#   build scripts produce by default); --nostreaming takes the plain builds instead.
#   zlib-ng and RALibretro have no streaming variant and always come from the plain tree.
#   --streaming is still accepted, as a no-op.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

STREAMING=1
case "${1:-}" in
    --nostreaming) STREAMING=0 ;;
    --streaming|"") ;;
    *) echo "error: unknown option '$1' (expected --nostreaming)" >&2; exit 2 ;;
esac

# --- platform/arch detection, matching native_lib.rs::platform_arch_dirs' submodule-
# facing names (linux/windows/mac/android/ios, x64/arm64) -- NOT the app-layout name
# (macos) used for the destination directory below; that mismatch is exactly what
# native_lib.rs's own os_dir_name()/platform_arch_dirs() consolidation documents.
# Every platform is assembled on every run, not just the one being run on: builds happen
# per platform (on that machine or cross-compiled), but the app layout carries all of them,
# and a Linux machine assembling a Windows tree is just a file copy. The host is still
# identified, but only to decide which platform's OpenSSL the cargo config points at.
case "$(uname -s)" in
    Linux) HOST_OS="linux" ;;
    Darwin) HOST_OS="mac" ;;
    MINGW*|MSYS*|CYGWIN*) HOST_OS="windows" ;;
    *) HOST_OS="" ;;
esac
case "$(uname -m)" in
    x86_64|amd64) HOST_ARCH="x64" ;;
    arm64|aarch64) HOST_ARCH="arm64" ;;
    *) HOST_ARCH="" ;;
esac

app_os_name() {
    case "$1" in
        mac) echo "macos" ;;
        *) echo "$1" ;;
    esac
}

# app_root_ex/lib/<os>/<arch>/ -- the arch level matches native_lib::arch_dir_name(), and is
# what lets one app root carry x64 and arm64 builds of the same OS side by side.
app_dest_root() {
    echo "$ROOT_DIR/app_root_ex/lib/$(app_os_name "$1")/$2"
}

# Which <os>/<arch> pairs any submodule has actually built. Filtered to the OS names
# native_lib::os_dir_name() knows, so a stray directory (zlib-ng keeps a `_build/` beside
# its release tree) is never mistaken for a platform.
discover_platforms() {
    local d os archd
    for d in "$ROOT_DIR"/deps/*/release; do
        [[ -d "$d" ]] || continue
        for os in linux mac windows android ios; do
            for archd in "$d/$os"/*/ "$d/with_streaming/$os"/*/; do
                [[ -d "$archd" ]] || continue
                echo "$os/$(basename "${archd%/}")"
            done
        done
    done | sort -u
}

# Latest (by mtime) version directory under <release_root>/<platform>/<arch>/, mirroring
# native_lib::find_latest_version_dir's own rule -- whichever build actually ran most
# recently, regardless of whether its version string sorts meaningfully. `ls -t` (not
# `find -printf`, which is GNU-only and absent on macOS's stock BSD find) sorts
# newest-first on both GNU and BSD systems, so this stays portable to Linux and macOS.
latest_version_dir() {
    local release_root="$1" platform="$2" arch="$3"
    local base="$release_root/$platform/$arch"
    [[ -d "$base" ]] || return 1
    # Version directories only. RALibretro's tree keeps an unversioned `include/` and
    # `dynamic/` beside its `20260927/`, and picking the newest child blindly lands on one of
    # those and then fails to find the library inside it.
    #
    # Two shapes: a leading digit (2.3.90, 4.2.0, 11.8.3, 20260927) and 7-Zip-zstd's
    # `v`-prefixed `v26.02-v1.5.7-R2-55-g3adff3e5`. Matching only `[0-9]*` skipped every
    # 7-Zip build, so `7z.so` was never deployed from the submodule tree. The non-version
    # siblings (include, dynamic, static, _build) match neither shape. Mirrors
    # native_lib::is_version_dir.
    local newest
    # `|| true`: with two globs, ls exits non-zero whenever one of them matches nothing, which
    # is the normal case (only 7-Zip-zstd uses v-prefixed versions). Under `set -e` that would
    # abort the whole deploy.
    newest="$(ls -1dt "$base"/[0-9]*/ "$base"/v[0-9]*/ 2>/dev/null | head -1 || true)"
    # Second tier: a build directory whose name is neither of those shapes. The windows
    # streaming builds land in `dev/` (no --version given) and dolphin's in a commit hash
    # `bd6ae6e6f4/`; both are real builds, and the shape allowlist above skipped them, so
    # those libraries were silently never deployed and the stale non-streaming DLLs from an
    # earlier run stayed in place. Anything that is not one of the known non-version
    # siblings counts. Kept as a fallback rather than folded into the first tier so a tree
    # holding both a real version and a leftover `dev/` still prefers the version.
    if [[ -z "$newest" ]]; then
        local d name
        while IFS= read -r d; do
            name="$(basename "${d%/}")"
            case "$name" in
                include|dynamic|static|bin|lib|_build|vendor|share|shared) continue ;;
            esac
            newest="$d"; break
        done < <(ls -1dt "$base"/*/ 2>/dev/null || true)
    fi
    # Not every tree is versioned: RALibretro's mac/arm64 build is just `dynamic/` and
    # `include/` directly under the arch. Fall back to the arch directory itself, which is
    # then the "version" the caller appends `dynamic/<lib>` to.
    [[ -n "$newest" ]] || { echo "$base"; return 0; }
    echo "${newest%/}"
}

variant_subdir() {
    [[ "$STREAMING" -eq 1 ]] && echo "with_streaming" || echo ""
}

# Submodules whose build produced nothing for the platform being assembled.
#
# These used to be a `skip ...` line in the middle of forty others, which is how a tree went
# out missing libRAHasher.so without anyone noticing: every wrapper falls back silently and
# only reports itself unavailable much later, at the point someone tries to hash a ROM. A
# summary at the end, with the command that fixes it, costs nothing and is hard to miss.
MISSING_HOST=()
MISSING_OTHER=()
record_missing() {
    if [[ "$SUBMODULE_OS" == "$HOST_OS" && "$ARCH" == "$HOST_ARCH" ]]; then
        MISSING_HOST+=("$1")
    else
        MISSING_OTHER+=("$SUBMODULE_OS/$ARCH: $1")
    fi
}

# How to build one, for the summary. RALibretro speaks its own dialect (--target, arch
# implied); the rest go through the orchestrator.
build_hint() {
    case "$1" in
        RALibretro) echo "deps/RALibretro/build_rahasher.sh --target $HOST_OS --type dynamic" ;;
        *)          echo "scripts/build_native_deps.sh --only $1 $HOST_OS $HOST_ARCH" ;;
    esac
}

# One submodule producing exactly one runtime file -> flat under $DEST_ROOT.
copy_single_lib() {
    local name="$1" release_root="$2" rel_lib_path_fn="$3" has_variants="${4:-1}"
    local variant=""
    [[ "$has_variants" -eq 1 ]] && variant="$(variant_subdir)"
    local search_root="$release_root"
    [[ -n "$variant" ]] && search_root="$release_root/$variant"

    local version_dir
    if ! version_dir="$(latest_version_dir "$search_root" "$SUBMODULE_OS" "$ARCH")" || [[ -z "$version_dir" ]]; then
        echo "  skip $name: no build found under $search_root/$SUBMODULE_OS/$ARCH/" >&2
        record_missing "$name"
        return 0
    fi

    local lib_path
    lib_path="$("$rel_lib_path_fn" "$version_dir")"
    if [[ ! -f "$lib_path" ]]; then
        echo "  skip $name: expected file not found at $lib_path" >&2
        record_missing "$name"
        return 0
    fi

    cp -f "$lib_path" "$DEST_ROOT/$(basename "$lib_path")"
    echo "  $name -> $DEST_ROOT/$(basename "$lib_path")"

    # Also place the library under its SONAME when that differs from the deployed filename.
    #
    # Needed by 7z.so, which links zlib-ng at build time and so records `NEEDED libz.so.1`
    # (zlib-ng's SONAME) while this script deploys the file as `libz.so` -- the name the Rust
    # side dlopen()s by explicit path. Without the second name the loader skips our copy and
    # silently falls back to the *system* zlib, which is exactly the slower library the build
    # went to the trouble of replacing. Both names, so both consumers are satisfied.
    #
    # Every arm ends in `|| true`, and each reader is checked for first. This script
    # assembles *every* platform that has a build, not just the host's, so on Linux it will
    # reach a mac pass and ask for `otool` -- which is not there. With the failure swallowed
    # by `2>/dev/null`, `set -e` then killed the script on the spot, silently, after the last
    # platform: no SONAME alias, and no .cargo/config.toml either, because that is written
    # further down. A missing reader is a reason to say so and carry on, not to abort.
    local soname=""
    case "$SUBMODULE_OS" in
        win) ;;  # no SONAME concept; the DLL is found by its own filename
        mac)
            if command -v otool >/dev/null 2>&1; then
                soname="$(otool -D "$lib_path" 2>/dev/null | tail -1 | xargs -r basename || true)"
            else
                echo "  note: no otool here, so no SONAME alias for $name -- assemble the macos tree on macOS" >&2
            fi
            ;;
        *)
            if command -v objdump >/dev/null 2>&1; then
                soname="$(objdump -p "$lib_path" 2>/dev/null | awk '/SONAME/ {print $2}' || true)"
            else
                echo "  note: no objdump here, so no SONAME alias for $name (install binutils)" >&2
            fi
            ;;
    esac
    if [[ -n "$soname" && "$soname" != "$(basename "$lib_path")" ]]; then
        cp -f "$lib_path" "$DEST_ROOT/$soname"
        echo "  $name -> $DEST_ROOT/$soname (SONAME)"
    fi
}

# The per-OS file names. A function, not a one-time `case`, because every platform is
# processed in the same shell: a mac pass must not leave `.dylib` names behind for a
# linux pass that follows it, so the linux arm is spelled out rather than left as the
# top-level default.
set_lib_paths_for_os() {
    case "$1" in
        mac)
            chdman_lib_path() { echo "$1/dynamic/libchdman.dylib"; }
            dolphin_lib_path() { echo "$1/dynamic/libdolphinrvz.dylib"; }
            zlibng_lib_path() { echo "$1/dynamic/libz.dylib"; }
            rahasher_lib_path() { echo "$1/dynamic/libRAHasher.dylib"; }
            sevenzip_lib_path() { echo "$1/7z.dylib"; }
            rxaac_lib_path() { echo "$1/dynamic/librxaac.dylib"; }
            rvgm_lib_path() { echo "$1/dynamic/librvgmstream.dylib"; }
            rommusic_lib_path() { echo "$1/dynamic/librommusic.dylib"; }
            cemuwua_lib_path() { echo "$1/dynamic/libcemuwua.dylib"; }
            exiso_lib_path() { echo "$1/dynamic/libexiso.dylib"; }
            psxpbp_lib_path() { echo "$1/dynamic/libpsxpbp.dylib"; }
            ;;
        windows)
            chdman_lib_path() { echo "$1/dynamic/chdman.dll"; }
            dolphin_lib_path() { echo "$1/dynamic/dolphinrvz.dll"; }
            zlibng_lib_path() { echo "$1/dynamic/zlib.dll"; }
            rahasher_lib_path() { echo "$1/dynamic/libRAHasher.dll"; } # keeps its lib prefix on Windows, unlike chdman/dolphinrvz (see hash.rs)
            sevenzip_lib_path() { echo "$1/7z.dll"; }
            rxaac_lib_path() { echo "$1/dynamic/rxaac.dll"; }
            rvgm_lib_path() { echo "$1/dynamic/rvgmstream.dll"; }
            rommusic_lib_path() { echo "$1/dynamic/rommusic.dll"; }
            cemuwua_lib_path() { echo "$1/dynamic/cemuwua.dll"; }
            exiso_lib_path() { echo "$1/dynamic/exiso.dll"; }
            psxpbp_lib_path() { echo "$1/dynamic/psxpbp.dll"; }
            ;;
        *)
            chdman_lib_path() { echo "$1/dynamic/libchdman.so"; }
            dolphin_lib_path() { echo "$1/dynamic/libdolphinrvz.so"; }
            zlibng_lib_path() { echo "$1/dynamic/libz.so"; }
            rahasher_lib_path() { echo "$1/dynamic/libRAHasher.so"; }
            sevenzip_lib_path() { echo "$1/7z.so"; } # flat, no dynamic/ subdir -- this build's own layout
            rxaac_lib_path() { echo "$1/dynamic/librxaac.so"; }
            rvgm_lib_path() { echo "$1/dynamic/librvgmstream.so"; }
            rommusic_lib_path() { echo "$1/dynamic/librommusic.so"; }
            cemuwua_lib_path() { echo "$1/dynamic/libcemuwua.so"; }
            exiso_lib_path() { echo "$1/dynamic/libexiso.so"; }
            psxpbp_lib_path() { echo "$1/dynamic/libpsxpbp.so"; }
            ;;
    esac
}

# Everything below assembles ONE platform, from globals SUBMODULE_OS / ARCH / DEST_ROOT.
process_platform() {
    SUBMODULE_OS="$1"
    ARCH="$2"
    DEST_ROOT="$(app_dest_root "$SUBMODULE_OS" "$ARCH")"
    mkdir -p "$DEST_ROOT"
    set_lib_paths_for_os "$SUBMODULE_OS"
    echo
    echo "=== $SUBMODULE_OS/$ARCH -> app_root_ex/lib/$(app_os_name "$SUBMODULE_OS")/$ARCH/ ==="

echo "Single-lib submodules:"
copy_single_lib "chdman-simd" "$ROOT_DIR/deps/chdman-simd/release" chdman_lib_path
copy_single_lib "dolphin" "$ROOT_DIR/deps/dolphin/release" dolphin_lib_path
copy_single_lib "zlib-ng" "$ROOT_DIR/deps/zlib-ng/release" zlibng_lib_path 0
copy_single_lib "RALibretro" "$ROOT_DIR/deps/RALibretro/release" rahasher_lib_path 0
copy_single_lib "7-Zip-zstd" "$ROOT_DIR/deps/7-Zip-zstd/release" sevenzip_lib_path
copy_single_lib "libxaac" "$ROOT_DIR/deps/libxaac/release" rxaac_lib_path 0
copy_single_lib "vgmstream" "$ROOT_DIR/deps/vgmstream/release" rvgm_lib_path 0
copy_single_lib "rommusic" "$ROOT_DIR/deps/rommusic/release" rommusic_lib_path 0
copy_single_lib "cemu" "$ROOT_DIR/deps/cemu/release" cemuwua_lib_path 0
copy_single_lib "extract-xiso" "$ROOT_DIR/deps/extract-xiso/release" exiso_lib_path 0
copy_single_lib "beetle-psx-libretro" "$ROOT_DIR/deps/beetle-psx-libretro/release" psxpbp_lib_path 0

# --- mariadb group: mariadb-server (bin/shared/share) + mariadb-connector-c (shared) +
# openssl (shared) combined into one tree, reproducing today's by-hand assembly.
echo "mariadb group:"
MARIADB_DEST="$DEST_ROOT/mariadb"
MDB_VERSION_DIR="$(latest_version_dir "$ROOT_DIR/deps/mariadb-server/release" "$SUBMODULE_OS" "$ARCH" || true)"
if [[ -z "${MDB_VERSION_DIR:-}" ]]; then
    echo "  skip mariadb-server: no build found for $SUBMODULE_OS/$ARCH" >&2
else
    mkdir -p "$MARIADB_DEST/bin" "$MARIADB_DEST/shared"
    [[ -d "$MDB_VERSION_DIR/bin" ]] && cp -f "$MDB_VERSION_DIR"/bin/* "$MARIADB_DEST/bin/" 2>/dev/null || true
    [[ -d "$MDB_VERSION_DIR/shared" ]] && cp -f "$MDB_VERSION_DIR"/shared/* "$MARIADB_DEST/shared/" 2>/dev/null || true
    echo "  mariadb-server bin/+shared/ -> $MARIADB_DEST/"

    # share/ -- the bootstrap SQL, share/english/errmsg.sys and share/charsets/, without
    # which mariadbd cannot create a database and `database::server::check_layout` refuses
    # to start. It is the one part of the tree that has no durable home:
    # build_mariadb_server.sh's collect_artifacts promotes bin/, lib*/ and include/ into
    # deps/mariadb-server/release/<platform>/<arch>/<version>/ and skips share/ entirely, so the only copy
    # lives in the CMake install-staging tree under deps/mariadb-server/_build/ -- which is scratch, and
    # which each build's own `rm -rf "${stage_dir}"` deletes before it starts.
    #
    # That is why Windows ships without it: not because the Windows build fails to produce
    # share/ (cmake --install does), but because nothing ever moves it somewhere that
    # survives the next build. The fix belongs in collect_artifacts -- CODING_RULES.md says
    # so under "Native binaries and app layout" -- and once it lands, the release tree below
    # is where share/ will be found. Both locations are checked so this keeps working either
    # way, in that order, newest home first.
    MDB_SHARE_SRC=""
    for candidate in \
        "$MDB_VERSION_DIR/share" \
        "$ROOT_DIR/deps/mariadb-server/_build/_stage/$SUBMODULE_OS-$ARCH/share"
    do
        # Not merely present: an empty or SQL-less share/ is as useless as none at all, and
        # finding that out here beats finding it out from a package gate, or from a first
        # start that cannot bootstrap.
        if [[ -d "$candidate" ]] && [[ -n "$(find "$candidate" -maxdepth 1 -name '*.sql' -print -quit 2>/dev/null)" ]]; then
            MDB_SHARE_SRC="$candidate"
            break
        fi
    done

    if [[ -n "$MDB_SHARE_SRC" ]]; then
        mkdir -p "$MARIADB_DEST/share"
        cp -rf "$MDB_SHARE_SRC"/. "$MARIADB_DEST/share/"
        echo "  mariadb-server share/ (from ${MDB_SHARE_SRC#"$ROOT_DIR"/}) -> $MARIADB_DEST/share/"
    else
        # Loud, and on stdout as well, because the old one-line stderr note scrolled past in
        # a long build and the next sign of trouble was a package gate much later -- or a
        # deployed tree that only failed when someone first ran it.
        MISSING_SHARE=1
        echo "  !! mariadb share/ NOT DEPLOYED for $SUBMODULE_OS/$ARCH"
        echo "     Looked in:"
        echo "       $MDB_VERSION_DIR/share"
        echo "       deps/mariadb-server/_build/_stage/$SUBMODULE_OS-$ARCH/share"
        echo "     Without it mariadbd cannot bootstrap a database and the server will not start."
        echo "     Rebuild mariadb-server for this platform and deploy without cleaning in"
        echo "     between, or promote share/ in its collect_artifacts (see CODING_RULES.md)."
    fi
fi

CONNECTOR_VERSION_DIR="$(latest_version_dir "$ROOT_DIR/deps/mariadb-connector-c/release" "$SUBMODULE_OS" "$ARCH" || true)"
if [[ -n "${CONNECTOR_VERSION_DIR:-}" && -d "$CONNECTOR_VERSION_DIR/shared" ]]; then
    mkdir -p "$MARIADB_DEST/shared"
    cp -f "$CONNECTOR_VERSION_DIR"/shared/* "$MARIADB_DEST/shared/" 2>/dev/null || true
    echo "  mariadb-connector-c shared/ -> $MARIADB_DEST/shared/"
else
    echo "  skip mariadb-connector-c: no build found for $SUBMODULE_OS/$ARCH" >&2
fi

OPENSSL_VERSION_DIR="$(latest_version_dir "$ROOT_DIR/deps/openssl/release" "$SUBMODULE_OS" "$ARCH" || true)"
if [[ -n "${OPENSSL_VERSION_DIR:-}" && -d "$OPENSSL_VERSION_DIR/shared" ]]; then
    mkdir -p "$MARIADB_DEST/shared"
    cp -f "$OPENSSL_VERSION_DIR"/shared/* "$MARIADB_DEST/shared/" 2>/dev/null || true
    echo "  openssl shared/ -> $MARIADB_DEST/shared/"
else
    echo "  skip openssl: no build found for $SUBMODULE_OS/$ARCH" >&2
fi

# openssl again, this time as a self-contained prefix the *build* can point at. The copy
# above is the runtime one: the embedded MariaDB needs libssl/libcrypto sitting beside its
# own binaries. This one adds the headers next to a second copy of the libs, because the
# `openssl-sys` crate (pulled in by the Dioxus desktop tree -- the rest of this project is
# rustls) needs OPENSSL_INCLUDE_DIR + OPENSSL_LIB_DIR and cannot read mariadb's layout.
# Two duplicated shared libraries in git-ignored build output is worth not coupling the
# build to where mariadb happens to keep its runtime files.
OPENSSL_DEST="$DEST_ROOT/openssl"
if [[ -n "${OPENSSL_VERSION_DIR:-}" && -d "$OPENSSL_VERSION_DIR/include" ]]; then
    mkdir -p "$OPENSSL_DEST/include" "$OPENSSL_DEST/lib"
    cp -rf "$OPENSSL_VERSION_DIR"/include/. "$OPENSSL_DEST/include/"
    cp -f "$OPENSSL_VERSION_DIR"/shared/* "$OPENSSL_DEST/lib/" 2>/dev/null || true
    echo "  openssl include/+lib/ -> $OPENSSL_DEST/ (build prefix)"
else
    OPENSSL_DEST=""
    echo "  skip openssl build prefix: no include/ in ${OPENSSL_VERSION_DIR:-<no build>}" >&2
fi

    # Only the platform we are running on can be built against here.
    if [[ "$SUBMODULE_OS" == "$HOST_OS" && "$ARCH" == "$HOST_ARCH" && -n "$OPENSSL_DEST" ]]; then
        HOST_OPENSSL_DEST="$OPENSSL_DEST"
        # The app-layout spelling (`macos`, not the submodules' `mac`), for the relative
        # rpath below -- it has to match what native_lib::os_dir_name() looks for.
        HOST_APP_OS="$(app_os_name "$SUBMODULE_OS")"
        HOST_APP_ARCH="$ARCH"
    fi
}

# --- every platform that has a build, one pass each ---------------------------------------
HOST_OPENSSL_DEST=""
HOST_APP_OS=""
HOST_APP_ARCH=""
PLATFORMS="$(discover_platforms)"
if [[ -z "$PLATFORMS" ]]; then
    echo "error: no built platforms found under deps/*/release/" >&2
    exit 1
fi
echo "Platforms with builds: $(echo "$PLATFORMS" | paste -sd' ' -) (streaming: $STREAMING)"

while IFS=/ read -r plat_os plat_arch; do
    [[ -n "$plat_os" ]] || continue
    process_platform "$plat_os" "$plat_arch"
done <<< "$PLATFORMS"

echo

# Point cargo at that prefix, so `cargo build` just works with no exported variables.
# Written rather than checked in because the path is machine- and OS-specific: cargo's
# [env] table has no per-OS form, and app_root_ex/lib/<os>/ differs on every platform.
# .cargo/config.toml is git-ignored for the same reason app_root_ex/lib/<os>/ is.
if [[ -n "$HOST_OPENSSL_DEST" ]]; then
    # `|| true`: under pipefail a rustc that exists but fails (a broken toolchain, or one an
    # antivirus has quarantined) would otherwise end the script here instead of skipping.
    HOST_TRIPLE="$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')" || true
    # Native Windows cargo cannot read an MSYS path like /d/...; give it D:/... instead.
    CARGO_OPENSSL_DEST="$HOST_OPENSSL_DEST"
    if [[ "$HOST_APP_OS" == "windows" ]] && command -v cygpath >/dev/null 2>&1; then
        CARGO_OPENSSL_DEST="$(cygpath -m "$HOST_OPENSSL_DEST")"
    fi
    if [[ -z "$HOST_TRIPLE" ]]; then
        echo "  skip cargo config: rustc not on PATH" >&2
    else
        mkdir -p "$ROOT_DIR/.cargo"
        # `$ORIGIN` on ELF, `@loader_path` on Mach-O: the same idea, spelled differently.
        case "$HOST_APP_OS" in
            macos) ORIGIN_TOKEN="@loader_path" ;;
            *)     ORIGIN_TOKEN="\$ORIGIN" ;;
        esac
        REL_LIB="lib/$HOST_APP_OS/$HOST_APP_ARCH/openssl/lib"
        {
            echo "# Generated by scripts/move_libs_to_app_root.sh -- do not edit, re-run the script."
            echo "# Points openssl-sys at this project's own OpenSSL build instead of a system one."
            echo
            echo "[env]"
            echo "OPENSSL_INCLUDE_DIR = \"$CARGO_OPENSSL_DEST/include\""
            echo "OPENSSL_LIB_DIR = \"$CARGO_OPENSSL_DEST/lib\""
            # PE has no rpath, and MSVC's link.exe rejects -Wl,... outright. Windows finds the
            # OpenSSL DLLs next to the executable (package.sh puts them there) or on PATH;
            # there is nothing to encode in the binary.
            [[ "$HOST_APP_OS" == "windows" ]] || {
            echo
            echo "# So a binary built here finds those libraries at run time too: they are not in any"
            echo "# system search path, and the versions here need not match anything installed."
            echo "# Host target only -- a linker rpath argument would break the wasm build."
            echo "#"
            echo "# Two entries, tried in order, because the same binary has to run from two"
            echo "# places. The first is relative to the executable and is what makes a packaged"
            echo "# build work at all: a shipped binary sits at the top of its package, beside"
            echo "# lib/. The second is this working copy, so \`cargo run\` keeps working. A"
            echo "# missing rpath entry costs the loader a stat() and nothing else."
            echo "#"
            echo "# Only the GUI needs any of this -- romexplorer_server links no OpenSSL at all."
            echo "# scripts/package.sh overrides RUSTFLAGS to drop the second entry, so a shipped"
            echo "# artifact carries no build-machine path."
            echo "[target.$HOST_TRIPLE]"
            echo "rustflags = ["
            echo "    \"-C\", \"link-arg=-Wl,-rpath,$ORIGIN_TOKEN/$REL_LIB\","
            echo "    \"-C\", \"link-arg=-Wl,-rpath,$HOST_OPENSSL_DEST/lib\","
            echo "]"
            }
        } > "$ROOT_DIR/.cargo/config.toml"
        echo "  cargo config -> .cargo/config.toml (OPENSSL_* + relative and dev rpaths for $HOST_TRIPLE)"
    fi
fi

if [[ ${#MISSING_HOST[@]} -gt 0 ]]; then
    echo
    echo "=== incomplete for this machine ($HOST_OS/$HOST_ARCH) ===" >&2
    for name in "${MISSING_HOST[@]}"; do
        printf '  %-20s build it with: %s\n' "$name" "$(build_hint "$name")" >&2
    done
    echo >&2
    echo "Then re-run this script. 'romexplorer_dbtool check' reports the same thing from an" >&2
    echo "installed copy." >&2
fi
if [[ ${#MISSING_OTHER[@]} -gt 0 ]]; then
    echo
    echo "Not built for other platforms (expected unless you cross-build here):"
    printf '  %s\n' "${MISSING_OTHER[@]}"
fi

# A tree without share/ looks complete -- mariadbd is there, the plugins are there -- and
# fails only when something tries to start it. Saying so once more at the very end, where
# the eye lands, is the difference between fixing it now and shipping it.
if [[ "${MISSING_SHARE:-0}" -eq 1 ]]; then
    echo
    echo "=== WARNING: the deployed mariadb tree has no share/ ===" >&2
    echo "It will pass a glance and fail at first start. scripts/package.sh refuses to" >&2
    echo "archive it, which is the next place this will come up." >&2
fi

echo "Done."
