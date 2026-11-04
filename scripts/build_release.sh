#!/usr/bin/env bash
# One command for a local release: native libraries, the GUI's web build, every requested
# package, the desktop bundles the host can make, and one SHA256SUMS -- in the order those
# steps depend on each other, which is written down nowhere else.
#
#   scripts/build_release.sh                                   # this host, server+desktop
#   scripts/build_release.sh --platform windows --arch x64,arm64 --mode server
#   scripts/build_release.sh --skip-native --mode server       # libraries already deployed
#
# Selectors take comma-separated lists and expand to their cartesian product. The natives are
# built once per (platform, arch) and the wasm build once per run -- never per mode.
#
# Order, and why:
#   1. preflight  build_native_deps.sh --check per (platform, arch), so a missing tool or an
#                 unverified arm64 target is reported before anything takes an hour.
#   2. natives    build_native_deps.sh per (platform, arch). It deploys into
#                 app_root_ex/lib/<os>/<arch>/, which package.sh reads.
#   3. gui_web    build_gui_web.sh: one wasm build, served by every server package.
#   4. packages   package.sh per (platform, arch, mode) -> release/<version>/<os>/<arch>/<mode>/
#                 and release/<version>/dist/.
#   5. bundles    appimage.sh for linux desktop, macos_app.sh for macOS desktop, where the
#                 host can run them.
#   5b. android   build_chdlite_android.sh --release --cli: the chdlite app (APK) and its
#                 command-line tool (chdlite alone) for android/arm64, into release/<version>/dist/ as
#                 chdlite-<v>-android-arm64[-unsigned].apk and chdlite-cli-<v>-android-arm64.zip.
#                 On whenever the Android SDK, NDK and JDK are found (see that script's
#                 --check); skipped with the reason otherwise. Uses the libraries already in
#                 app_root_ex/lib/android/arm64/ (build_chdlite_android.sh --natives makes them).
#   6. SHA256SUMS over release/<version>/dist/, checkable with `sha256sum -c`.
#
# A combination this host cannot build (another OS; an arch cargo cannot target here) is
# skipped with the reason, not attempted. The run exits non-zero if every requested
# combination was skipped, or if any step failed (with --continue, after trying the rest).
#
# Options:
#   --platform LIST   linux,windows,macos           (default: this host)
#   --arch LIST       x64,arm64                     (default: this host's)
#   --mode LIST       server,desktop,cli            (default: server,desktop). cli is the
#                     standalone chdlite tools, into release/cli/<chdlite version>/
#   --version V       (default: the workspace version)
#   --no-clean        pass --no-clean to the native builds
#   --skip-native     use the libraries already in app_root_ex/lib/
#   --native-only     build and deploy the libraries, then stop
#   --core            natives only for what the server needs (openssl, zlib-ng, 7-Zip,
#                     both mariadbs) -- no chdman, dolphin or RAHasher
#   --no-gui-web      skip the wasm build (server packages then have no /app/)
#   --jobs N          build parallelism for the native builds and cargo
#   --continue        keep going after a failed step and report them all at the end
#   --unverified-arm64  passed to build_native_deps.sh (see its SUPPORTS_ARM64_* maps)
#   --android / --no-android   force the Android step on (fail if the toolchain is missing)
#                     or off (default: on when the toolchain is found)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPTS="$ROOT_DIR/scripts"

PLATFORMS="" ARCHES="" MODES="server,desktop" VERSION=""
CLEAN=1 SKIP_NATIVE=0 NATIVE_ONLY=0 CORE=0 GUI_WEB=1 JOBS="" CONTINUE=0 UNVERIFIED=0 ANDROID=auto

usage() {
    sed -n '2,/^[^#]/p' "${BASH_SOURCE[0]}" | sed -n 's/^# \{0,1\}//p'
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --platform) PLATFORMS="${2:-}"; shift 2 ;;
        --arch) ARCHES="${2:-}"; shift 2 ;;
        --mode) MODES="${2:-}"; shift 2 ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --no-clean) CLEAN=0; shift ;;
        --skip-native) SKIP_NATIVE=1; shift ;;
        --native-only) NATIVE_ONLY=1; shift ;;
        --core) CORE=1; shift ;;
        --no-gui-web) GUI_WEB=0; shift ;;
        --jobs) JOBS="${2:-}"; shift 2 ;;
        --continue) CONTINUE=1; shift ;;
        --unverified-arm64) UNVERIFIED=1; shift ;;
        --android) ANDROID=1; shift ;;
        --no-android) ANDROID=0; shift ;;
        -h|--help) usage 0 ;;
        *) echo "unknown option: $1" >&2; usage 2 ;;
    esac
done
[[ "$SKIP_NATIVE" -eq 1 && "$NATIVE_ONLY" -eq 1 ]] && { echo "error: --skip-native and --native-only contradict each other" >&2; exit 2; }

# --- host, in the app layout's spelling (package.sh's), and the submodules' -------------------
case "$(uname -s)" in
    Linux) HOST_OS="linux" ;;
    Darwin) HOST_OS="macos" ;;
    MINGW*|MSYS*|CYGWIN*) HOST_OS="windows" ;;
    *) echo "error: unknown host OS $(uname -s)" >&2; exit 2 ;;
esac
case "$(uname -m)" in
    arm64|aarch64) HOST_ARCH="arm64" ;;
    *) HOST_ARCH="x64" ;;
esac
PLATFORMS="${PLATFORMS:-$HOST_OS}"
ARCHES="${ARCHES:-$HOST_ARCH}"

# An MSYS2 shell does not inherit the Windows PATH unless MSYS2_PATH_TYPE=inherit, so cargo and
# a Python installed the Windows way are invisible to it. If either is missing, append the
# Windows PATH (machine, then user -- read from the registry, since the shell has already
# dropped it), which is what `inherit` would have done; exported, so every step below sees it.
if [[ "$HOST_OS" == "windows" ]] && ! { command -v cargo && command -v python3; } >/dev/null 2>&1; then
    win_path="$(powershell.exe -NoProfile -Command "[Environment]::GetEnvironmentVariable('Path','Machine') + ';' + [Environment]::GetEnvironmentVariable('Path','User')" 2>/dev/null | tr -d '\r')" || win_path=""
    [[ -n "$win_path" ]] && export PATH="$PATH:$(cygpath -u -p "$win_path")"
fi
# The WindowsApps directory holds app execution aliases, and its python3.exe comes before a real
# install on a default user PATH. Launched from an MSYS2 terminal (a pty) the alias never
# starts -- `python3 -c 'print(1)'` hangs forever, while the same file runs fine with no
# terminal attached -- so the directory is dropped and the real interpreter is found instead.
if [[ "$HOST_OS" == "windows" ]]; then
    PATH="$(tr ':' '\n' <<< "$PATH" | grep -v -i '/Microsoft/WindowsApps/*$' | paste -sd: -)"
    export PATH
fi
for t in cargo python3; do
    command -v "$t" >/dev/null 2>&1 \
        || { echo "error: $t is not on PATH$([[ "$HOST_OS" == windows ]] && echo " -- in MSYS2, start the shell with MSYS2_PATH_TYPE=inherit to keep your Windows PATH")" >&2; exit 2; }
done

split() { tr ',' '\n' <<< "$1" | sed '/^$/d'; }
mapfile -t P_LIST < <(split "$PLATFORMS")
mapfile -t A_LIST < <(split "$ARCHES")
mapfile -t M_LIST < <(split "$MODES")
for p in "${P_LIST[@]}"; do case "$p" in linux|windows|macos) ;; mac) echo "error: say macos, not mac" >&2; exit 2 ;; *) echo "error: unknown platform '$p'" >&2; exit 2 ;; esac; done
for a in "${A_LIST[@]}"; do case "$a" in x64|arm64) ;; *) echo "error: unknown arch '$a'" >&2; exit 2 ;; esac; done
for m in "${M_LIST[@]}"; do case "$m" in server|desktop|cli) ;; *) echo "error: unknown mode '$m'" >&2; exit 2 ;; esac; done

if [[ -z "$VERSION" ]]; then
    VERSION="$(cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT_DIR/Cargo.toml" 2>/dev/null \
        | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="romexplorer"))')" \
        || { echo "error: couldn't read the workspace version; pass --version" >&2; exit 2; }
fi
DIST="$ROOT_DIR/release/$VERSION/dist"
[[ -n "$JOBS" ]] && export CARGO_BUILD_JOBS="$JOBS"

# --- bookkeeping ----------------------------------------------------------------------------
declare -a TIMINGS=() SKIPPED=() FAILED=()
RUN_START=$SECONDS

# Runs a step, timing it. On failure: stop, or with --continue record it and carry on.
step() {
    local label="$1"; shift
    local start=$SECONDS
    echo
    echo "=== $label ==="
    if "$@"; then
        TIMINGS+=("$(printf '  %-40s %5ds' "$label" $((SECONDS - start)))")
        return 0
    fi
    TIMINGS+=("$(printf '  %-40s %5ds  FAILED' "$label" $((SECONDS - start)))")
    FAILED+=("$label")
    if [[ "$CONTINUE" -eq 0 ]]; then
        summary
        exit 1
    fi
    return 1
}

summary() {
    echo
    echo "=== release $VERSION: $((SECONDS - RUN_START))s ==="
    [[ ${#TIMINGS[@]} -gt 0 ]] && printf '%s\n' "${TIMINGS[@]}"
    if [[ ${#SKIPPED[@]} -gt 0 ]]; then
        echo "skipped:"
        printf '  %s\n' "${SKIPPED[@]}"
    fi
    if [[ ${#FAILED[@]} -gt 0 ]]; then
        echo "failed:"
        printf '  %s\n' "${FAILED[@]}"
    fi
    [[ -d "$DIST" ]] && { echo "dist: ${DIST#$ROOT_DIR/}"; ls -1 "$DIST" | sed 's/^/  /'; }
}

# Whether this host can produce a (platform, arch) at all, and if not, why not.
why_not() {
    local p="$1" a="$2"
    if [[ "$p" != "$HOST_OS" ]]; then
        echo "a $p build needs a $p host (the native libraries and cargo target are host-built here)"
        return
    fi
    # Same OS, other arch: needs the rust target; everything else is build_native_deps.sh's
    # own business, and its preflight says so.
    if [[ "$a" != "$HOST_ARCH" ]]; then
        local triple
        case "$p/$a" in
            windows/arm64) triple="aarch64-pc-windows-msvc" ;;
            windows/x64) triple="x86_64-pc-windows-msvc" ;;
            linux/arm64) triple="aarch64-unknown-linux-gnu" ;;
            linux/x64) triple="x86_64-unknown-linux-gnu" ;;
            macos/arm64) triple="aarch64-apple-darwin" ;;
            macos/x64) triple="x86_64-apple-darwin" ;;
        esac
        if ! rustup target list --installed 2>/dev/null | grep -qx "$triple"; then
            echo "rust target $triple is not installed (rustup target add $triple)"
            return
        fi
    fi
}

native_platform() { [[ "$1" == "macos" ]] && echo mac || echo "$1"; }
native_flags=()
[[ "$CLEAN" -eq 0 ]] && native_flags+=(--no-clean)
[[ "$CORE" -eq 1 ]] && native_flags+=(--only openssl,zlib-ng,7-Zip-zstd,mariadb-connector-c,mariadb-server)
# chdlite on its own loads two libraries; openssl is still needed to build the romexplorer
# crate it depends on.
[[ "$MODES" == "cli" && "$CORE" -eq 0 ]] && native_flags+=(--only openssl,chdman-simd,dolphin)
[[ -n "$JOBS" ]] && native_flags+=(--jobs "$JOBS")
[[ "$UNVERIFIED" -eq 1 ]] && native_flags+=(--unverified-arm64)

# --- 1. targets and preflight ---------------------------------------------------------------
TARGETS=()
for p in "${P_LIST[@]}"; do
    for a in "${A_LIST[@]}"; do
        reason="$(why_not "$p" "$a")"
        if [[ -n "$reason" ]]; then
            SKIPPED+=("$p/$a: $reason")
            continue
        fi
        if [[ "$SKIP_NATIVE" -eq 0 ]]; then
            # build_native_deps.sh's own refusal (an unverified arm64 arm, a missing tool) is
            # the best statement of what is missing, so it is shown verbatim.
            if ! out="$(bash "$SCRIPTS/build_native_deps.sh" "${native_flags[@]}" --check "$(native_platform "$p")" "$a" 2>&1)"; then
                SKIPPED+=("$p/$a: native preflight refused:"$'\n'"$(sed 's/^/      /' <<< "$out" | grep -v '^\s*$')")
                continue
            fi
        elif [[ ! -d "$ROOT_DIR/app_root_ex/lib/$p/$a" ]]; then
            SKIPPED+=("$p/$a: --skip-native, but app_root_ex/lib/$p/$a does not exist")
            continue
        fi
        TARGETS+=("$p/$a")
    done
done

if [[ ${#TARGETS[@]} -eq 0 ]]; then
    echo "nothing to build: every requested combination was skipped" >&2
    summary
    exit 1
fi
echo "release $VERSION: targets ${TARGETS[*]}; modes ${M_LIST[*]}"

# --- 2. natives -------------------------------------------------------------------------------
if [[ "$SKIP_NATIVE" -eq 0 ]]; then
    for t in "${TARGETS[@]}"; do
        p="${t%/*}" a="${t#*/}"
        step "natives $t" bash "$SCRIPTS/build_native_deps.sh" "${native_flags[@]}" "$(native_platform "$p")" "$a" || true
    done
fi
if [[ "$NATIVE_ONLY" -eq 1 ]]; then
    summary
    [[ ${#FAILED[@]} -eq 0 ]]
    exit
fi

# --- 3. the GUI's web build, once ---------------------------------------------------------------
wants_web=0
for m in "${M_LIST[@]}"; do [[ "$m" == "server" ]] && wants_web=1; done
if [[ "$GUI_WEB" -eq 1 && "$wants_web" -eq 1 ]]; then
    step "gui_web (wasm, once)" bash "$SCRIPTS/build_gui_web.sh" || true
fi

# --- 4. packages, 5. desktop bundles ---------------------------------------------------------
# Both GUIs link WebKitGTK -- romexplorer's desktop and chdlite-gui, which the cli package
# carries (dioxus desktop) -- so a linux/arm64 cross build of either needs its arm64
# development files. scripts/linux_arm64_sysroot.sh unpacks them into a private sysroot
# (installing the :arm64 -dev packages would uninstall the x64 ones); package.sh picks it up.
# Built here on first need, so a fresh machine does not have to know to run it; no root.
# Without it cargo fails at link time, minutes in, and without --continue that would take the
# finished server package's SHA256SUMS down with it -- so a failure skips only those packages.
ARM64_SYSROOT="${LINUX_ARM64_SYSROOT:-$ROOT_DIR/tests/output/sysroot/linux-arm64}"
ARM64_GUI_BLOCKER=""
needs_arm64_webkit=0
for t in "${TARGETS[@]}"; do
    [[ "$t" == "linux/arm64" ]] || continue
    for m in "${M_LIST[@]}"; do [[ "$m" != "server" ]] && needs_arm64_webkit=1; done
done
if [[ "$needs_arm64_webkit" -eq 1 && "$HOST_ARCH" != "arm64" \
      && ! -f "$ARM64_SYSROOT/.complete" \
      && ! -f /usr/lib/aarch64-linux-gnu/pkgconfig/webkit2gtk-4.1.pc ]]; then
    echo
    echo "=== arm64 sysroot (once: no arm64 WebKitGTK yet) ==="
    start=$SECONDS
    if bash "$SCRIPTS/linux_arm64_sysroot.sh"; then
        TIMINGS+=("$(printf '  %-40s %5ds' "arm64 sysroot" $((SECONDS - start)))")
    else
        TIMINGS+=("$(printf '  %-40s %5ds  FAILED' "arm64 sysroot" $((SECONDS - start)))")
        ARM64_GUI_BLOCKER="no arm64 WebKitGTK to link against, and scripts/linux_arm64_sysroot.sh failed (its error is above)"
    fi
fi
if [[ -z "$ARM64_GUI_BLOCKER" && "$needs_arm64_webkit" -eq 1 && "$HOST_ARCH" != "arm64" ]] \
   && ! command -v "${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER:-aarch64-linux-gnu-gcc}" >/dev/null 2>&1; then
    ARM64_GUI_BLOCKER="no arm64 cross compiler (sudo apt install gcc-aarch64-linux-gnu g++-aarch64-linux-gnu)"
fi

for t in "${TARGETS[@]}"; do
    p="${t%/*}" a="${t#*/}"
    for m in "${M_LIST[@]}"; do
        if [[ "$t" == "linux/arm64" && "$m" != "server" && -n "$ARM64_GUI_BLOCKER" ]]; then
            SKIPPED+=("$t $m: $ARM64_GUI_BLOCKER")
            continue
        fi
        if [[ "$m" == "cli" ]]; then
            # No --version: the cli tools carry their own (chdlite/Cargo.toml), not the workspace's.
            step "package $t cli" bash "$SCRIPTS/package.sh" --kind cli --os "$p" --arch "$a" || true
            continue
        fi
        step "package $t $m" bash "$SCRIPTS/package.sh" --kind "$m" --os "$p" --arch "$a" --version "$VERSION" || continue
        if [[ "$m" == "desktop" ]]; then
            stage="$ROOT_DIR/release/$VERSION/$p/$a/desktop"
            case "$p" in
                linux)
                    if [[ "$HOST_OS" == "linux" ]]; then
                        step "appimage $t" bash "$SCRIPTS/appimage.sh" --stage "$stage" --arch "$a" --version "$VERSION" || true
                    fi ;;
                macos)
                    if [[ "$HOST_OS" == "macos" ]]; then
                        step "macos .app $t" bash "$SCRIPTS/macos_app.sh" --stage "$stage" --arch "$a" --version "$VERSION" || true
                    fi ;;
            esac
        fi
    done
done

# --- 5b. android: the chdlite app and its command-line tools ------------------------------------
# Unsigned unless chdlite/Dioxus.toml has an [android.signing] keystore (then Gradle writes
# app-release.apk); the file name says which.
android_release() {
    bash "$SCRIPTS/build_chdlite_android.sh" --release --cli || return 1
    local cli_version apk_dir apk name cli_dir zip
    cli_version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$ROOT_DIR/chdlite/Cargo.toml" | head -1)"
    apk_dir="$ROOT_DIR/target/dx/chdlite-gui/release/android/app/app/build/outputs/apk/release"
    if [[ -f "$apk_dir/app-release.apk" ]]; then
        apk="$apk_dir/app-release.apk"; name="chdlite-$cli_version-android-arm64.apk"
    else
        apk="$apk_dir/app-release-unsigned.apk"; name="chdlite-$cli_version-android-arm64-unsigned.apk"
    fi
    [[ -f "$apk" ]] || { echo "error: no release APK in $apk_dir" >&2; return 1; }
    mkdir -p "$DIST"
    cp -f "$apk" "$DIST/$name"
    # chdlite alone, one file with its libraries inside (chdcomp and the rest are the same program
    # under another name, which a phone's terminal has no use for); deflate (what a phone's file
    # manager opens), the executable bit kept for an unzip in a terminal.
    cli_dir="$ROOT_DIR/release/cli/$cli_version/android/arm64"
    zip="$DIST/chdlite-cli-$cli_version-android-arm64.zip"
    python3 - "$cli_dir" "$zip" <<'PY' || return 1
import os, sys, zipfile
src, out = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as z:
    for name in ("chdlite",):
        info = zipfile.ZipInfo.from_file(os.path.join(src, name), name)
        info.external_attr = 0o755 << 16
        info.compress_type = zipfile.ZIP_DEFLATED
        with open(os.path.join(src, name), "rb") as f:
            z.writestr(info, f.read())
PY
    for f in "$DIST/$name" "$zip"; do
        (cd "$DIST" && sha256sum "$(basename "$f")" > "$(basename "$f").sha256")
    done
    echo "android: $name, $(basename "$zip")"
}
if [[ "$ANDROID" != 0 && "$NATIVE_ONLY" -eq 0 ]]; then
    if [[ ! -f "$ROOT_DIR/app_root_ex/lib/android/arm64/libchdman.so" ]]; then
        reason="no android/arm64 libraries in app_root_ex/lib/android/arm64 (scripts/build_chdlite_android.sh --natives)"
    elif ! out="$(bash "$SCRIPTS/build_chdlite_android.sh" --check 2>&1)"; then
        reason="the Android toolchain is incomplete:"$'\n'"$(grep '^  - ' <<< "$out" | sed 's/^/    /')"
    else
        reason=""
    fi
    if [[ -n "$reason" && "$ANDROID" == 1 ]]; then
        echo "error: --android, but $reason" >&2
        FAILED+=("android: $reason")
    elif [[ -n "$reason" ]]; then
        SKIPPED+=("android/arm64: $reason")
    else
        step "android/arm64 chdlite (apk + cli)" android_release || true
    fi
fi

# --- 6. SHA256SUMS -------------------------------------------------------------------------------
# One file over everything in dist/, in the format `sha256sum -c` reads; the per-archive
# .sha256 files package.sh writes stay alongside for single downloads.
for DIST in "$DIST" "$ROOT_DIR"/release/cli/*/dist; do
if [[ -d "$DIST" ]]; then
    (
        cd "$DIST"
        # A glob, not find -printf: that is GNU-only, and macOS builds run this too.
        files=()
        for f in *; do
            [[ -f "$f" && "$f" != SHA256SUMS && "$f" != *.sha256 ]] && files+=("$f")
        done
        [[ ${#files[@]} -gt 0 ]] && sha256sum "${files[@]}" > SHA256SUMS
        sha256sum -c --quiet SHA256SUMS
    ) && echo "SHA256SUMS: $(wc -l < "$DIST/SHA256SUMS") files, verified ($DIST)"
fi
done

summary
[[ ${#FAILED[@]} -eq 0 ]]
