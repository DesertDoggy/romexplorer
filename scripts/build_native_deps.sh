#!/usr/bin/env bash
# Builds every native dependency submodule, in dependency order, then deploys the results
# into app_root_ex/ via scripts/move_libs_to_app_root.sh.
#
# For occasional RELEASE builds, not the inner development loop: it builds all eight
# submodules every run and stops at the first failure. To rebuild one thing while working on
# it, call that submodule's own script directly, or use --only.
#
# Why this exists rather than a README list of commands:
#
#   * Three argument dialects. `--platform <p>` (openssl, zlib-ng, the two mariadbs),
#     `[--nostreaming] [<p> <arch>]` (7-Zip, chdman, dolphin), and `--target/--type`
#     (RALibretro, whose script is not even in user/scripts/). There is no uniform loop to
#     write; each recipe below builds its own argv.
#   * Order is load-bearing and the failures are late. 7-Zip links against zlib-ng's release
#     tree; both mariadb builds discover OpenSSL from its release tree and never fall back to
#     a system one. Build them out of order and you find out several minutes in.
#   * OPENSSL_ROOT_DIR has to be handed over explicitly -- see openssl_root() below.
#
# Usage: scripts/build_native_deps.sh [options] [<platform> <arch>]
#   --nostreaming     build (and deploy) the plain variant of 7-Zip/chdman/dolphin instead of
#                     the streaming one. Passed to the deploy step too, so what is built and
#                     what is deployed cannot disagree.
#   --no-clean        pass --no-clean to the submodules that default to a scratch rebuild
#                     (openssl, zlib-ng, mariadb-connector-c, mariadb-server).
#   --no-deploy       build only; leave app_root_ex/ alone.
#   --only <a,b,...>  build just these (comma-separated submodule names, as listed by --list).
#   --jobs N          build parallelism, exported as JOBS. Honoured by openssl, zlib-ng and
#                     the two mariadb builds; 7-Zip, chdman, dolphin and RALibretro call nproc
#                     themselves and ignore it. All of them default to the host CPU count.
#   --list            print the build order and exit.
#   --check           run the preflight checks and exit without building. Worth doing before
#                     committing to a full run, which takes the better part of an hour.
#   --unverified-arm64  build linux/windows arm64 with submodules whose arm64 arm exists but
#                     has not been verified yet (see the SUPPORTS_ARM64_* maps).
#   <platform> <arch> target to build for; defaults to the detected host. `macos` is
#                     accepted for `mac`. Only combinations every selected submodule
#                     supports are accepted -- see SUPPORTS_ARM64_*.
#
# Never runs git and never installs anything. Writes build output only under deps/<name>/
# (release/, _build/, logs/) and app_root_ex/, both git-ignored. The sources are the upstream
# submodules under external/; a few builds apply deps/<name>/patches/ to their checkout, which
# then shows as modified inside that submodule (see deps/README.md).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
LOG_DIR="$ROOT_DIR/tests/output/build-deps"

STREAMING=1
CLEAN=1
DEPLOY=1
ONLY=""
JOBS=""
LIST_ONLY=0
CHECK_ONLY=0
UNVERIFIED_ARM64=0
POSITIONAL=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --nostreaming) STREAMING=0 ;;
        --streaming) ;;  # accepted as a no-op, matching move_libs_to_app_root.sh
        --no-clean) CLEAN=0 ;;
        --no-deploy) DEPLOY=0 ;;
        --unverified-arm64) UNVERIFIED_ARM64=1 ;;
        --only) ONLY="${2:-}"; shift ;;
        --jobs) JOBS="${2:-}"; shift ;;
        --list) LIST_ONLY=1 ;;
        --check) CHECK_ONLY=1 ;;
        -h|--help) sed -n '2,38p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) echo "error: unknown option '$1' (try --help)" >&2; exit 2 ;;
        *) POSITIONAL+=("$1") ;;
    esac
    shift
done

# --- target -----------------------------------------------------------------------------
# Submodule-facing names (linux/mac/windows, x64/arm64), the same vocabulary
# move_libs_to_app_root.sh and native_lib.rs::platform_arch_dirs use. The app layout's
# `macos` spelling only appears in the destination, which the deploy script handles.
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

# Windows on ARM: the MSYS2 runtime is an x64 program running under emulation, so uname -m
# above says x86_64 and PROCESSOR_ARCHITECTURE reads AMD64 -- Windows rewrites it for every
# emulated process. The MSVC-driven scripts (openssl, both mariadbs, dolphin) choose
# `vcvarsall arm64` over `x64_arm64` from that variable, so on an arm64 machine they took the
# cross arm: the x64-hosted cl.exe under emulation (24.6s against 14.2s native for the same
# six compiles here), and for mariadb-server a whole extra x64 build of its host tools first
# -- 3h40m in all. The kernel name is the one thing emulation does not disguise
# (MSYS_NT-10.0-26300-ARM64), and an exported value does survive into the child MSYS shells,
# which is where those scripts read it.
#
# HOST_ARCH stays x64 on purpose: for the MinGW-driven builds this shell really is an x64
# one, and their llvm-mingw cross arm is already right (that clang is a native arm64 binary).
WIN_NATIVE_ARM64=0
if [[ "$HOST_OS" == "windows" && "$(uname -s)" == *-ARM64* ]]; then
    WIN_NATIVE_ARM64=1
    export PROCESSOR_ARCHITECTURE=ARM64
fi

if [[ ${#POSITIONAL[@]} -eq 2 ]]; then
    PLATFORM="${POSITIONAL[0]}"
    ARCH="${POSITIONAL[1]}"
elif [[ ${#POSITIONAL[@]} -eq 0 ]]; then
    PLATFORM="$HOST_OS"
    ARCH="$HOST_ARCH"
else
    echo "error: give both <platform> and <arch>, or neither" >&2
    exit 2
fi
# The workflow and the app layout say `macos`; every submodule script says `mac`.
[[ "$PLATFORM" == "macos" ]] && PLATFORM="mac"
if [[ -z "$PLATFORM" || -z "$ARCH" ]]; then
    echo "error: could not detect host platform/arch ($(uname -s) $(uname -m)); pass them explicitly" >&2
    exit 2
fi

# These scripts cross-compile only with toolchains preset in the environment
# (ANDROID_NDK_HOME, IOS_TOOLCHAIN_FILE, WINDOWS_TOOLCHAIN_FILE, ...). Building a target that
# is not the host is therefore something you opt into knowingly, so say so rather than
# failing obscurely inside whichever submodule notices first.
if [[ -n "$HOST_OS" && "$PLATFORM/$ARCH" != "$HOST_OS/$HOST_ARCH" ]]; then
    echo "note: $PLATFORM/$ARCH is not this host ($HOST_OS/$HOST_ARCH) -- cross-toolchain env must already be set" >&2
fi

# The four sh-dialect scripts clean by default, which is what a release build wants.
clean_flag() { [[ "$CLEAN" -eq 1 ]] || echo "--no-clean"; }
stream_flag() { [[ "$STREAMING" -eq 1 ]] || echo "--nostreaming"; }

# --- the build order --------------------------------------------------------------------
# openssl and zlib-ng first because things link against their *release trees*; everything
# after them is free. Kept as one list so --list and the run agree by construction.
ORDER=(openssl zlib-ng 7-Zip-zstd mariadb-connector-c mariadb-server chdman-simd dolphin RALibretro libxaac cemu extract-xiso beetle-psx-libretro)

# Whether a submodule has been *verified* to build linux/arm64 or windows/arm64 -- by an
# actual build whose output was checked to be AArch64, not by reading the script. Every
# submodule script now has an arm64 arm for both (`--arch arm64`, or `<platform> arm64`);
# a 0 here means that arm has not produced a checked binary yet. Everything supports the x64
# targets and mac/ios/android arm64. Refusing up front beats a late, obscure failure --
# --unverified-arm64 overrides it, which is how an entry gets to 1 in the first place.
#
# 7-Zip needs no edit of its own: its Makefile takes CC/CXX/RC from the environment
# (cross_env below) and it links zlib-ng's release tree for the same arch.
declare -A SUPPORTS_ARM64_LINUX=(
    [openssl]=1
    [zlib-ng]=1
    [7-Zip-zstd]=1
    [mariadb-connector-c]=1
    [mariadb-server]=1
    [chdman-simd]=1
    [dolphin]=1
    [RALibretro]=1
    [libxaac]=1
    [cemu]=0
    [extract-xiso]=0
    [beetle-psx-libretro]=0
)
declare -A SUPPORTS_ARM64_WINDOWS=(
    [openssl]=1
    [zlib-ng]=1
    [7-Zip-zstd]=1
    [mariadb-connector-c]=1
    [mariadb-server]=1
    [chdman-simd]=1
    [dolphin]=1
    [RALibretro]=1
    [libxaac]=1
    [cemu]=0
    [extract-xiso]=0
    [beetle-psx-libretro]=0
)

# Cross compilers for the make-driven builds (7-Zip) when the target arch is not the host's.
# The cmake-driven scripts pick their own from --arch; these three take them from the
# environment. Printed as env(1) assignments.
cross_env() {
    [[ "$ARCH" == "arm64" && "$HOST_ARCH" != "arm64" ]] || return 0
    case "$PLATFORM" in
        windows) echo "CC=aarch64-w64-mingw32-clang CXX=aarch64-w64-mingw32-clang++ RC=aarch64-w64-mingw32-windres" ;;
        linux)
            local p="${LINUX_ARM64_CROSS_PREFIX:-aarch64-linux-gnu-}"
            echo "CC=${p}gcc CXX=${p}g++" ;;
    esac
}

selected() {
    [[ -z "$ONLY" ]] && return 0
    [[ ",$ONLY," == *",$1,"* ]]
}

SELECTED=()
for name in "${ORDER[@]}"; do
    selected "$name" && SELECTED+=("$name")
done
if [[ ${#SELECTED[@]} -eq 0 ]]; then
    echo "error: --only '$ONLY' matched nothing (see --list)" >&2
    exit 2
fi

if [[ "$LIST_ONLY" -eq 1 ]]; then
    echo "build order for $PLATFORM/$ARCH:"
    for name in "${SELECTED[@]}"; do echo "  $name"; done
    [[ "$DEPLOY" -eq 1 ]] && echo "then: scripts/move_libs_to_app_root.sh $(stream_flag)"
    exit 0
fi

# --- preflight ----------------------------------------------------------------------------
# All of it before any build starts. With "build everything, stop on the first failure", the
# worst outcome is learning twenty minutes into MariaDB that bison is missing.
PREFLIGHT_ERRORS=()
need_tool() {
    command -v "$1" >/dev/null 2>&1 || PREFLIGHT_ERRORS+=("missing '$1' -- needed by $2")
}
# Pure bash on purpose: a version of this that piped through grep would, if grep were ever
# unavailable, report "not selected" and silently skip that submodule's preflight checks --
# failing open is the wrong direction for a check whose whole job is to fail early.
is_selected() {
    local want="$1" have
    for have in "${SELECTED[@]}"; do [[ "$have" == "$want" ]] && return 0; done
    return 1
}

# Whether the bison on PATH is new enough for mariadb-server (>= 2.4).
bison_ok() {
    local v major minor
    v="$(bison --version 2>/dev/null | sed -n '1s/.* //p')" || return 1
    major="${v%%.*}"; minor="${v#*.}"; minor="${minor%%.*}"
    [[ "$major" =~ ^[0-9]+$ && "$minor" =~ ^[0-9]+$ ]] || return 1
    [[ "$major" -gt 2 || ( "$major" -eq 2 && "$minor" -ge 4 ) ]]
}

echo "=== preflight: $PLATFORM/$ARCH, $([[ $STREAMING -eq 1 ]] && echo streaming || echo plain) ==="

# arm64 gate, before anything else: this is a refusal, not a warning.
if [[ "$ARCH" == "arm64" && ( "$PLATFORM" == "linux" || "$PLATFORM" == "windows" ) ]]; then
    blocked=()
    for name in "${SELECTED[@]}"; do
        if [[ "$PLATFORM" == "linux" ]]; then ok="${SUPPORTS_ARM64_LINUX[$name]}"; else ok="${SUPPORTS_ARM64_WINDOWS[$name]}"; fi
        [[ "$ok" -eq 0 ]] && blocked+=("$name")
    done
    if [[ ${#blocked[@]} -gt 0 && "$UNVERIFIED_ARM64" -eq 0 ]]; then
        echo >&2
        echo "error: $PLATFORM/$ARCH has not been verified yet for: ${blocked[*]}" >&2
        echo >&2
        echo "Their build scripts have an arm64 arm, but it has not produced a checked AArch64" >&2
        echo "binary on this target yet. Pass --unverified-arm64 to try anyway, then flip the entry" >&2
        echo "in SUPPORTS_ARM64_$(echo "$PLATFORM" | tr a-z A-Z) once the output checks out." >&2
        exit 1
    fi
    [[ ${#blocked[@]} -gt 0 ]] && echo "  note: building unverified $PLATFORM/arm64 arms: ${blocked[*]}"
fi

# windows/arm64 cross-compiles the MinGW-built libraries (zlib-ng, 7-Zip, chdman, RAHasher)
# with llvm-mingw; pick up its standard install location if it is not on PATH already.
if [[ "$PLATFORM/$ARCH" == "windows/arm64" && "$HOST_ARCH" != "arm64" ]] \
   && ! command -v aarch64-w64-mingw32-clang >/dev/null 2>&1; then
    for d in "${LLVM_MINGW_ROOT:-}" /c/llvm-mingw; do
        [[ -n "$d" && -x "$d/bin/aarch64-w64-mingw32-clang.exe" ]] && { export PATH="$d/bin:$PATH"; break; }
    done
fi

need_tool cmake "most submodule builds"
# The make-driven builds call plain `make` with POSIX paths and recipes. A default MSYS2 has
# none (pacman -S make), and the mingw32-make that Strawberry Perl and llvm-mingw put on PATH
# is not found under that name -- without this, 7-Zip dies one second in, twenty minutes
# after openssl started.
for name in 7-Zip-zstd chdman-simd RALibretro; do
    is_selected "$name" || continue
    need_tool make "$name$([[ "$HOST_OS" == windows ]] && echo " (MSYS2: pacman -S make)")"
    break
done
if [[ "$PLATFORM/$ARCH" == "windows/arm64" && "$HOST_ARCH" != "arm64" ]]; then
    for name in zlib-ng 7-Zip-zstd chdman-simd RALibretro libxaac; do
        is_selected "$name" || continue
        command -v aarch64-w64-mingw32-clang >/dev/null 2>&1 || PREFLIGHT_ERRORS+=("missing llvm-mingw (aarch64-w64-mingw32-clang) -- needed by $name for windows/arm64; unpack https://github.com/mstorsjo/llvm-mingw to C:\\llvm-mingw or set LLVM_MINGW_ROOT")
        break
    done
    # Not on an arm64 machine: there the arm64 tools it generates run as built, so there is
    # no x64 pass and nothing for an x64 OpenSSL to do.
    if [[ "$WIN_NATIVE_ARM64" -eq 0 ]] && is_selected mariadb-server && ! ls -d "$ROOT_DIR"/deps/openssl/release/windows/x64/*/ >/dev/null 2>&1; then
        PREFLIGHT_ERRORS+=("mariadb-server windows/arm64 builds its generated tools for x64 first and needs the windows/x64 OpenSSL build for that -- run a windows x64 build (or --only openssl windows x64) first")
    fi
fi
if [[ "$PLATFORM/$ARCH" == "linux/arm64" && "$HOST_ARCH" != "arm64" ]]; then
    lp="${LINUX_ARM64_CROSS_PREFIX:-aarch64-linux-gnu-}"
    need_tool "${lp}g++" "linux/arm64 cross builds (apt: g++-aarch64-linux-gnu)"
    if is_selected mariadb-server; then
        need_tool qemu-aarch64 "mariadb-server linux/arm64 (runs its generated build tools; apt: qemu-user)"
        [[ -e "/usr/lib/${lp%-}/libncurses.so" ]] || PREFLIGHT_ERRORS+=("missing /usr/lib/${lp%-}/libncurses.so -- mariadb-server linux/arm64 needs libncurses-dev:arm64 (dpkg --add-architecture arm64)")
    fi
fi
is_selected openssl && need_tool perl "openssl"
if is_selected mariadb-server; then
    # No pre-generated parser is shipped, and Curses is FIND_PACKAGE(... REQUIRED).
    # macOS ships bison 2.3 in /usr/bin and Homebrew's is keg-only (never linked onto PATH),
    # so `brew install bison` alone changes nothing: CMake finds 2.3 and refuses three minutes
    # in. Put the keg first for every step below rather than asking for a PATH prefix.
    if [[ "$HOST_OS" == "mac" ]] && ! bison_ok; then
        for d in "${HOMEBREW_PREFIX:-/opt/homebrew}/opt/bison/bin" /opt/homebrew/opt/bison/bin /usr/local/opt/bison/bin; do
            [[ -x "$d/bison" ]] && { export PATH="$d:$PATH"; echo "  using $d/bison ($(/usr/bin/bison --version 2>/dev/null | sed -n '1s/.* //p') in /usr/bin is too old)"; break; }
        done
    fi
    if command -v bison >/dev/null 2>&1 && ! bison_ok; then
        PREFLIGHT_ERRORS+=("bison $(bison --version | sed -n '1s/.* //p') at $(command -v bison) is too old -- mariadb-server needs >= 2.4$([[ "$HOST_OS" == mac ]] && echo " (brew install bison)")")
    fi
    need_tool bison "mariadb-server (>= 2.4; there is no pre-generated parser in tree)$([[ "$HOST_OS" == windows && -x /c/msys64/usr/bin/bison.exe ]] && echo " -- MSYS2 has it; run this from an MSYS2 MINGW64 shell (MSYS2_PATH_TYPE=inherit keeps your Windows PATH)")"
    if [[ "$PLATFORM" == "linux" ]] && ! ls /usr/include/curses.h /usr/include/ncurses.h >/dev/null 2>&1; then
        PREFLIGHT_ERRORS+=("missing curses/readline headers -- mariadb-server's CMake requires them (libncurses-dev)")
    fi
fi
if is_selected chdman-simd; then
    # chdman extracts MAME sources for a specific commit it resolves from APPLY_PATCHES.md.
    # Without those objects fetched it exits 3 with a message about a missing "Bumped version
    # to X" commit, which does not hint that the fix is a (large) git fetch.
    # MAME_SOURCE_DIR points it at another clone; only its history is read, so that one may
    # be a no-checkout clone.
    mame_dir="${MAME_SOURCE_DIR:-$ROOT_DIR/external/mame}"
    if ! git -C "$mame_dir" rev-parse --git-dir >/dev/null 2>&1; then
        PREFLIGHT_ERRORS+=("${mame_dir#$ROOT_DIR/} is not checked out -- chdman-simd extracts MAME sources from it (a large fetch), or set MAME_SOURCE_DIR to a MAME clone")
    fi
fi
if is_selected dolphin && { [[ ! -f "$ROOT_DIR/external/dolphin/Externals/fmt/fmt/CMakeLists.txt" ]] \
        || [[ ! -f "$ROOT_DIR/external/dolphin/Externals/cubeb/cubeb/cmake/sanitizers-cmake/CMakeLists.txt" ]]; }; then
    # dolphin's Externals/ are submodules of dolphin itself, some two levels deep (cubeb's
    # sanitizers-cmake). A non-recursive checkout leaves them empty, and CMake then fails
    # with a dozen "does not contain a CMakeLists.txt".
    PREFLIGHT_ERRORS+=("external/dolphin's own submodules are not checked out -- git -C external/dolphin submodule update --init --recursive")
fi
if is_selected RALibretro && { [[ ! -f "$ROOT_DIR/external/RALibretro/src/miniz/miniz.c" ]] \
        || [[ ! -d "$ROOT_DIR/external/RALibretro/src/rcheevos/include" ]]; }; then
    # Same story as dolphin: miniz and rcheevos are RALibretro's own submodules, and make
    # otherwise stops at "No rule to make target .../miniz.o".
    PREFLIGHT_ERRORS+=("external/RALibretro's own submodules are not checked out -- git -C external/RALibretro submodule update --init --recursive")
fi
if is_selected cemu; then
    # Cemu's own dependencies are vcpkg ports and submodules of its own; cemuwua takes the
    # few it needs from external/ instead, so none of Cemu's submodules are needed.
    for f in cemu/src/Cafe/Filesystem/FST/FST.cpp ZArchive/src/zarchivewriter.cpp zstd/lib/zstd.h fmt/include/fmt/format.h; do
        [[ -f "$ROOT_DIR/external/$f" ]] && continue
        PREFLIGHT_ERRORS+=("external/${f%%/*} is not checked out -- git submodule update --init external/${f%%/*}")
    done
fi
if is_selected extract-xiso && [[ ! -f "$ROOT_DIR/external/extract-xiso/extract-xiso.c" ]]; then
    PREFLIGHT_ERRORS+=("external/extract-xiso is not checked out -- git submodule update --init external/extract-xiso")
fi
if is_selected beetle-psx-libretro && [[ ! -f "$ROOT_DIR/external/beetle-psx-libretro/mednafen/cdrom/CDAccess_PBP.c" ]]; then
    # Not recursive: libretro-common is in Beetle's own tree, and nothing else of it is used.
    PREFLIGHT_ERRORS+=("external/beetle-psx-libretro is not checked out -- git submodule update --init external/beetle-psx-libretro")
fi
if is_selected 7-Zip-zstd && [[ "$PLATFORM" == "windows" && "$ARCH" == "x64" ]]; then
    if ! command -v uasm >/dev/null 2>&1 && ! command -v jwasm >/dev/null 2>&1 \
       && ! command -v asmc >/dev/null 2>&1 && ! command -v ml64 >/dev/null 2>&1; then
        PREFLIGHT_ERRORS+=("no assembler (uasm/jwasm/asmc/ml64) on PATH -- 7-Zip needs one for windows/x64")
    fi
fi

for name in "${SELECTED[@]}"; do
    case "$name" in
        RALibretro) script="$ROOT_DIR/deps/RALibretro/build_rahasher.sh" ;;
        zlib-ng)    script="$ROOT_DIR/deps/zlib-ng/scripts/build-zlib-ng-release.sh" ;;
        7-Zip-zstd) script="$ROOT_DIR/deps/7-Zip-zstd/scripts/build.sh" ;;
        openssl)    script="$ROOT_DIR/deps/openssl/scripts/build_openssl.sh" ;;
        chdman-simd) script="$ROOT_DIR/deps/chdman-simd/scripts/build_chdman_lib.sh" ;;
        dolphin)    script="$ROOT_DIR/deps/dolphin/scripts/build_dolphin_rvz.sh" ;;
        mariadb-connector-c) script="$ROOT_DIR/deps/mariadb-connector-c/scripts/build_connector_c.sh" ;;
        mariadb-server) script="$ROOT_DIR/deps/mariadb-server/scripts/build_mariadb_server.sh" ;;
        libxaac)    script="$ROOT_DIR/deps/libxaac/scripts/build.sh" ;;
        cemu)       script="$ROOT_DIR/deps/cemu/scripts/build.sh" ;;
        extract-xiso) script="$ROOT_DIR/deps/extract-xiso/scripts/build.sh" ;;
        beetle-psx-libretro) script="$ROOT_DIR/deps/beetle-psx-libretro/scripts/build.sh" ;;
    esac
    [[ -f "$script" ]] || PREFLIGHT_ERRORS+=("$name: build script not found at ${script#$ROOT_DIR/} (deps/ incomplete?)")
done

if [[ ${#PREFLIGHT_ERRORS[@]} -gt 0 ]]; then
    echo >&2
    for e in "${PREFLIGHT_ERRORS[@]}"; do echo "error: $e" >&2; done
    echo >&2
    echo "Nothing was built. Fix the above and re-run." >&2
    exit 1
fi
echo "  ok: ${SELECTED[*]}"
if [[ "$CHECK_ONLY" -eq 1 ]]; then
    echo "(--check: nothing built)"
    exit 0
fi

# --- the freshly built OpenSSL, for the two mariadb builds --------------------------------
# build_mariadb_server.sh documents OPENSSL_ROOT_DIR as required and explicitly never falls
# back to a system OpenSSL; the connector auto-discovers but only on a best guess. Resolving
# it here -- newest version directory under the release tree -- is the one piece of state
# that has to cross between submodules, so it is handed over rather than hoped for.
openssl_root() {
    local base="$ROOT_DIR/deps/openssl/release/$PLATFORM/$ARCH"
    [[ -d "$base" ]] || return 1
    local newest
    newest="$(ls -1dt "$base"/*/ 2>/dev/null | head -1 || true)"
    [[ -n "$newest" ]] || return 1
    echo "${newest%/}"
}

# --- nothing from the build machine's shell ----------------------------------------------
# These are read by every compiler driver, configure script, CMake and pkg-config, so one line
# in a shell profile reaches all nine builds. It did: a ~/.zshrc exporting
# LDFLAGS=-L/opt/homebrew/opt/jpeg/lib (for an unrelated Pillow build) put that search path on
# every link here and into every CMakeCache.txt, ahead of the SDK's own. Nothing in this
# script wants a library the submodules do not bundle or build themselves, so drop them on
# every platform. Cross toolchains come in through their own variables (ANDROID_NDK_HOME,
# *_TOOLCHAIN_FILE, LINUX_ARM64_CROSS_PREFIX, MSVC's INCLUDE/LIB), which are left alone.
for v in LDFLAGS CPPFLAGS CFLAGS CXXFLAGS OBJCFLAGS OBJCXXFLAGS LIBRARY_PATH CPATH \
         C_INCLUDE_PATH CPLUS_INCLUDE_PATH OBJC_INCLUDE_PATH PKG_CONFIG_PATH CMAKE_PREFIX_PATH; do
    if [[ -n "${!v:-}" ]]; then
        echo "  ignoring $v from the environment (${!v})"
        unset "$v"
    fi
done

# --- running one submodule ----------------------------------------------------------------
mkdir -p "$LOG_DIR"
declare -a TIMINGS=()

fmt_elapsed() {
    if [[ "$1" -lt 60 ]]; then printf '%ds' "$1"; else printf '%dm%02ds' $(($1 / 60)) $(($1 % 60)); fi
}

# --- progress while a step runs -----------------------------------------------------------
# A step's output goes to its log, so without this the terminal shows a header and then
# nothing -- for twenty minutes, in mariadb-server's case, which is indistinguishable from the
# hang it once really was. A background ticker shows elapsed time and the newest log line
# (make's "[ 45%] Building ..." or ninja's "[123/4567]" carry the percentage for free):
# rewritten in place once a second on a terminal, appended every PROGRESS_INTERVAL seconds
# (default 30) when piped, so a CI log gets a heartbeat rather than a thousand lines.
#
# The ticker is the background job, not the build: a non-interactive shell starts background
# jobs with SIGINT ignored, so backgrounding the build would make Ctrl-C leave it running.
PROGRESS_PID=""
progress_start() {
    local log="$1" start=$SECONDS
    local interval="${PROGRESS_INTERVAL:-30}" tty=0
    [[ -t 1 ]] && tty=1
    (
        ticks=0 esc=$'\033'
        # $$ is still the script here; if it is gone (Ctrl-C), stop within a second.
        while kill -0 $$ 2>/dev/null; do
            sleep 1
            ticks=$((ticks + 1))
            [[ "$tty" -eq 1 || $((ticks % interval)) -eq 0 ]] || continue
            line="$(tail -c 4000 "$log" 2>/dev/null | tr '\r' '\n' | sed "s/${esc}\[[0-9;]*[A-Za-z]//g" \
                | LC_ALL=C tr -d '\000-\010\013-\037' | grep -v '^[[:space:]]*$' | tail -1 \
                | sed 's/^[[:space:]]*//' || true)"
            prefix="    $(fmt_elapsed $((SECONDS - start)))  "
            if [[ "$tty" -eq 1 ]]; then
                cols="$(tput cols 2>/dev/null || echo 80)"
                room=$((cols - ${#prefix} - 1))
                [[ "$room" -lt 0 ]] && room=0
                printf '\r\033[K%s%s' "$prefix" "${line:0:room}"
            else
                printf '%s%s\n' "$prefix" "${line:0:160}"
            fi
        done
    ) &
    PROGRESS_PID=$!
}
progress_stop() {
    [[ -n "$PROGRESS_PID" ]] || return 0
    kill "$PROGRESS_PID" 2>/dev/null || true
    wait "$PROGRESS_PID" 2>/dev/null || true
    PROGRESS_PID=""
    [[ -t 1 ]] && printf '\r\033[K'
    return 0
}

STEP_NO=0
run_step() {
    local name="$1"; shift
    local log="$LOG_DIR/$name.log"
    local start=$SECONDS rc=0
    STEP_NO=$((STEP_NO + 1))
    echo
    echo "=== $name ($STEP_NO/$STEP_TOTAL) ==="
    echo "    $*"
    echo "    log: ${log#$ROOT_DIR/}"
    : >"$log"
    progress_start "$log"
    "$@" >"$log" 2>&1 || rc=$?
    progress_stop
    if [[ "$rc" -ne 0 ]]; then
        local elapsed=$((SECONDS - start))
        echo >&2
        echo "error: $name failed after $(fmt_elapsed "$elapsed")" >&2
        echo "  command: $*" >&2
        echo "  log:     ${log#$ROOT_DIR/}" >&2
        echo "  --- last 20 lines ---" >&2
        tail -20 "$log" >&2
        exit 1
    fi
    local elapsed=$((SECONDS - start))
    TIMINGS+=("$(printf '  %-22s %4ds' "$name" "$elapsed")")
    echo "    done in $(fmt_elapsed "$elapsed")"
}

# Only the four sh-dialect scripts read it (JOBS=${JOBS:-<cpu count>}); the others use nproc.
[[ -n "$JOBS" ]] && export JOBS

build_one() {
    local name="$1"
    local sub="$ROOT_DIR/deps"
    case "$name" in
        openssl)
            run_step "$name" sh "$sub/openssl/scripts/build_openssl.sh" --platform "$PLATFORM" --arch "$ARCH" $(clean_flag)
            ;;
        zlib-ng)
            run_step "$name" sh "$sub/zlib-ng/scripts/build-zlib-ng-release.sh" --platform "$PLATFORM" --arch "$ARCH" $(clean_flag)
            ;;
        7-Zip-zstd)
            run_step "$name" env $(cross_env) bash "$sub/7-Zip-zstd/scripts/build.sh" $(stream_flag) "$PLATFORM" "$ARCH"
            ;;
        mariadb-connector-c)
            local root; root="$(openssl_root)" || { echo "error: mariadb-connector-c needs a built OpenSSL; run without --only, or --only openssl first" >&2; exit 1; }
            run_step "$name" env OPENSSL_ROOT_DIR="$root" sh "$sub/mariadb-connector-c/scripts/build_connector_c.sh" --platform "$PLATFORM" --arch "$ARCH" $(clean_flag)
            ;;
        mariadb-server)
            local root; root="$(openssl_root)" || { echo "error: mariadb-server needs a built OpenSSL (OPENSSL_ROOT_DIR); run without --only, or --only openssl first" >&2; exit 1; }
            run_step "$name" env OPENSSL_ROOT_DIR="$root" sh "$sub/mariadb-server/scripts/build_mariadb_server.sh" --platform "$PLATFORM" --arch "$ARCH" $(clean_flag)
            ;;
        chdman-simd)
            run_step "$name" bash "$sub/chdman-simd/scripts/build_chdman_lib.sh" $(stream_flag) "$PLATFORM" "$ARCH"
            ;;
        dolphin)
            run_step "$name" bash "$sub/dolphin/scripts/build_dolphin_rvz.sh" $(stream_flag) "$PLATFORM" "$ARCH"
            ;;
        RALibretro)
            # Its own dialect: --target/--arch, and no streaming variant.
            run_step "$name" bash "$sub/RALibretro/build_rahasher.sh" --target "$PLATFORM" --arch "$ARCH" --type dynamic
            ;;
        libxaac)
            # The AAC / xHE-AAC encoder behind a flat C shim (deps/libxaac/rxaac.h). No streaming
            # variant and nothing links against it; CC comes from cross_env like 7-Zip's.
            run_step "$name" env $(cross_env) bash "$sub/libxaac/scripts/build.sh" "$PLATFORM" "$ARCH"
            ;;
        cemu)
            # cemuwua: Wii U .wud/.wux into .wua through Cemu's own disc and FST code
            # (deps/cemu/cemuwua.h), with ZArchive, zstd and fmt from external/. No streaming
            # variant: reading a .wua back is streamed by the one build.
            run_step "$name" bash "$sub/cemu/scripts/build.sh" "$PLATFORM" "$ARCH"
            ;;
        extract-xiso)
            # exiso: an Xbox image rewritten as an XISO by extract-xiso's own code
            # (deps/extract-xiso/exiso.h). No streaming variant.
            run_step "$name" bash "$sub/extract-xiso/scripts/build.sh" "$PLATFORM" "$ARCH"
            ;;
        beetle-psx-libretro)
            # psxpbp: Beetle PSX's EBOOT.PBP reader (deps/beetle-psx-libretro/psxpbp.h),
            # for reading a PS1 disc out of a PBP. No streaming variant.
            run_step "$name" bash "$sub/beetle-psx-libretro/scripts/build.sh" "$PLATFORM" "$ARCH"
            ;;
    esac
}

TOTAL_START=$SECONDS
STEP_TOTAL=$((${#SELECTED[@]} + DEPLOY))
for name in "${SELECTED[@]}"; do
    build_one "$name"
done

if [[ "$DEPLOY" -eq 1 ]]; then
    run_step "deploy" bash "$SCRIPT_DIR/move_libs_to_app_root.sh" $(stream_flag)
fi

echo
echo "=== built $PLATFORM/$ARCH in $((SECONDS - TOTAL_START))s ==="
[[ ${#TIMINGS[@]} -gt 0 ]] && printf '%s\n' "${TIMINGS[@]}"
# `A || B; C` runs C unconditionally -- the `;` ends the || list -- so this used to print
# "not deployed" after every successful deploy, and dereference an unset `msg` while doing
# it, which `set -u` turns into an abort on the very last line of a good build.
if [[ "$DEPLOY" -eq 0 ]]; then
    echo "  (not deployed -- run scripts/move_libs_to_app_root.sh $(stream_flag))"
fi
