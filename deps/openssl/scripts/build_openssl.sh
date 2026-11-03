#!/bin/sh
# Builds OpenSSL (libssl + libcrypto) as shared libraries, out-of-tree, for one of
# mac/ios/android/linux/windows. This is what mariadb-connector-c's build script
# (deps/mariadb-connector-c/scripts/build_connector_c.sh) points its OPENSSL_ROOT_DIR
# at, so neither connector-c nor the mariadb-server build ever depends on a system-installed
# OpenSSL/GnuTLS -- same command, same result, on Linux, macOS and Windows.
#
# No source patch is needed: this uses OpenSSL's own perl Configure/make, fully out-of-tree
# (nothing is written back into the submodule's own tree), same as every other build script
# in this repo. Output lands under deps/openssl/release/<platform>/<arch>/<version>/{shared,include}.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
USER_DIR=$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)
REPO_ROOT=$(CDPATH= cd -- "${USER_DIR}/../.." && pwd)
# The upstream source tree (a submodule). This script, its patches and its build output
# (release/, _build/, logs/) live in deps/openssl/ instead.
ROOT_DIR="${OPENSSL_SOURCE_DIR:-${REPO_ROOT}/external/openssl}"
RELEASE_DIR="${USER_DIR}/release"
BUILD_ROOT="${USER_DIR}/_build"
STAGE_ROOT="${BUILD_ROOT}/_stage"
LOG_DIR="${USER_DIR}/logs"

PLATFORM=""
PLATFORM_SET=0
ARCH=""
CLEAN=1
VERSION_OVERRIDE=""

TIMESTAMP=$(date +%Y%m%d-%H%M%S)
LOG_FILE="${LOG_DIR}/build-openssl-${TIMESTAMP}.log"

mkdir -p "${LOG_DIR}" "${RELEASE_DIR}" "${BUILD_ROOT}" "${STAGE_ROOT}"
: > "${LOG_FILE}"

log_line() {
    level="$1"
    shift
    line="[${level}] $*"
    printf '%s\n' "${line}"
    printf '%s\n' "${line}" >> "${LOG_FILE}"
}

run_and_log() {
    log_line INFO "RUN: $*"
    # Streamed, not buffered until the command ends: a twenty-minute build that prints
    # nothing looks exactly like a hang, and scripts/build_native_deps.sh shows the newest
    # log line as progress. POSIX sh has no pipefail, so the status crosses the pipe in a file.
    rc_file="${LOG_DIR}/.rc-$$"
    rm -f "${rc_file}"
    { rc=0; "$@" 2>&1 || rc=$?; echo "${rc}" > "${rc_file}"; } | tee -a "${LOG_FILE}"
    rc=$(cat "${rc_file}" 2>/dev/null || echo 1)
    rm -f "${rc_file}"
    [ "${rc}" -eq 0 ] && return 0
    log_line ERROR "Command failed (exit=${rc}): $*"
    return "${rc}"
}

run_and_log_in() {
    # Same as run_and_log, but runs "$@" with cwd set to $1 first.
    dir="$1"; shift
    log_line INFO "RUN (in ${dir}): $*"
    rc_file="${LOG_DIR}/.rc-$$"
    rm -f "${rc_file}"
    { rc=0; ( cd "${dir}" && "$@" ) 2>&1 || rc=$?; echo "${rc}" > "${rc_file}"; } | tee -a "${LOG_FILE}"
    rc=$(cat "${rc_file}" 2>/dev/null || echo 1)
    rm -f "${rc_file}"
    [ "${rc}" -eq 0 ] && return 0
    log_line ERROR "Command failed (exit=${rc}): $*"
    return "${rc}"
}

usage() {
    cat << 'EOF'
Usage:
  sh deps/openssl/scripts/build_openssl.sh [options]

Options:
  --platform <mac|ios|android|linux|windows>
  --arch <x64|arm64>   linux/windows only (default: the host's); mac/ios/android are arm64
  --clean | --no-clean
  --version <value>
  --help

Environment variables:
  IOS_CROSS_TOP / IOS_CROSS_SDK  Optional for iOS; auto-detected via `xcrun` when unset
                                  (needs a macOS host with Xcode command line tools)
  ANDROID_NDK_HOME                Required for --platform android
  ANDROID_API                     Optional for Android (default: 24)
  WINDOWS_CROSS_PREFIX             Optional mingw-w64 cross prefix (default:
                                  x86_64-w64-mingw32-) used on a non-Windows host
  LINUX_X64_CROSS_PREFIX          Optional cross-compiler prefix for a Linux x64 cross build
  LINUX_ARM64_CROSS_PREFIX        Cross-compiler prefix for linux/arm64 from a non-arm64 host
                                  (default: aarch64-linux-gnu-, from gcc-aarch64-linux-gnu)
  JOBS                            Optional build parallelism (default: host CPU count)

Every target builds shared libssl/libcrypto, skips the test suite and docs (no-tests
no-docs), and installs only libs+headers (`make install_sw`) -- no system OpenSSL install is
ever touched or required.
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --platform)
            [ "$#" -ge 2 ] || { log_line ERROR "Missing value for --platform"; exit 2; }
            PLATFORM="$2"; PLATFORM_SET=1; shift 2 ;;
        --arch)
            [ "$#" -ge 2 ] || { log_line ERROR "Missing value for --arch"; exit 2; }
            ARCH="$2"; shift 2 ;;
        --clean) CLEAN=1; shift ;;
        --no-clean) CLEAN=0; shift ;;
        --version)
            [ "$#" -ge 2 ] || { log_line ERROR "Missing value for --version"; exit 2; }
            VERSION_OVERRIDE="$2"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) log_line ERROR "Unknown argument: $1"; usage; exit 2 ;;
    esac
done

if [ "${PLATFORM_SET}" -eq 1 ]; then
    case "${PLATFORM}" in
        mac|ios|android|linux|windows) ;;
        *) log_line ERROR "Invalid --platform value: ${PLATFORM}"; exit 2 ;;
    esac
else
    host_os=$(uname -s)
    case "${host_os}" in
        Darwin) PLATFORM="mac" ;;
        Linux) PLATFORM="linux" ;;
        MINGW*|MSYS*|CYGWIN*) PLATFORM="windows" ;;
        *) log_line ERROR "Unsupported host OS: ${host_os}. Use --platform to select a target explicitly."; exit 2 ;;
    esac
    log_line INFO "Auto-detected host platform '${PLATFORM}' from '${host_os}'."
fi

case "$(uname -m)" in
    arm64|aarch64) HOST_ARCH="arm64" ;;
    *) HOST_ARCH="x64" ;;
esac
case "${ARCH}" in
    "") case "${PLATFORM}" in mac|ios|android) ARCH="arm64" ;; *) ARCH="${HOST_ARCH}" ;; esac ;;
    x64|arm64) ;;
    aarch64) ARCH="arm64" ;;
    x86_64|amd64) ARCH="x64" ;;
    *) log_line ERROR "Invalid --arch value: ${ARCH} (expected x64 or arm64)"; exit 2 ;;
esac
case "${PLATFORM}/${ARCH}" in
    mac/arm64|ios/arm64|android/arm64|linux/x64|linux/arm64|windows/x64|windows/arm64) ;;
    *) log_line ERROR "Unsupported target ${PLATFORM}/${ARCH}. Supported: mac/arm64 ios/arm64 android/arm64 linux/x64 linux/arm64 windows/x64 windows/arm64"; exit 2 ;;
esac

if ! command -v perl >/dev/null 2>&1; then
    log_line ERROR "perl was not found. OpenSSL's build (Configure) needs it. Install perl and retry."
    exit 2
fi
# make is only needed by the Configure/make(1) targets below (mac/ios/android/linux, and a
# windows build cross-compiled from a non-Windows host via mingw-w64). A native-Windows
# windows build instead uses nmake, resolved from vcvarsall.bat inside build_windows_msvc --
# not expected on PATH ahead of time, so skip this check for that case.
native_windows_host=0
case "${PLATFORM}:$(uname -s)" in
    windows:MINGW*|windows:MSYS*|windows:CYGWIN*) native_windows_host=1 ;;
esac
if [ "${native_windows_host}" -eq 0 ] && ! command -v make >/dev/null 2>&1; then
    log_line ERROR "make was not found. Install make (or mingw32-make on Windows, on PATH as 'make') and retry."
    exit 2
fi

if [ -n "${VERSION_OVERRIDE}" ]; then
    VERSION="${VERSION_OVERRIDE}"
    log_line INFO "Using version override: ${VERSION}"
else
    VERSION=$(
        awk -F= '
            /^MAJOR=/ { maj=$2 }
            /^MINOR=/ { min=$2 }
            /^PATCH=/ { pat=$2 }
            END { if (maj != "" && min != "" && pat != "") print maj "." min "." pat }
        ' "${ROOT_DIR}/VERSION.dat" 2>/dev/null
    )
    if [ -z "${VERSION}" ]; then
        VERSION=$(git -C "${ROOT_DIR}" describe --tags 2>/dev/null || date +%Y%m%d-%H%M%S)
        log_line FALLBACK "Could not read VERSION.dat. Using ${VERSION}."
    else
        log_line INFO "Using repo version: ${VERSION}"
    fi
fi

if [ "${CLEAN}" -eq 1 ]; then
    log_line INFO "Cleaning build/stage roots"
    rm -rf "${BUILD_ROOT}" "${STAGE_ROOT}"
    mkdir -p "${BUILD_ROOT}" "${STAGE_ROOT}"
fi

JOBS_DEFAULT=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)
JOBS=${JOBS:-${JOBS_DEFAULT}}

# mac only. OpenSSL stamps each dylib with its --prefix as install name, and that prefix is
# the scratch stage dir under deps/openssl/_build/. Everything linked against these libraries (both
# mariadbs, the GUI through openssl-sys) copies that id into its own load commands, so it
# would only ever run on this machine, and only until the next clean build. Rewritten here,
# before anything links: the id becomes @rpath/<name> (consumers already carry an rpath to
# wherever the libraries are deployed beside them), and libssl's own reference to libcrypto
# becomes @loader_path/<name>, since the two always travel together and libssl has no rpath.
# install_name_tool invalidates the signature arm64 requires, hence the ad-hoc re-sign.
relocate_macos_dylibs() {
    shared_dir="$1"
    stage_lib="$2"
    for f in "${shared_dir}"/*.dylib; do
        [ -f "${f}" ] && [ ! -L "${f}" ] || continue
        install_name_tool -id "@rpath/$(basename "${f}")" "${f}" || return 1
        otool -L "${f}" | awk 'NR > 1 { print $1 }' | while IFS= read -r dep; do
            case "${dep}" in
                "${stage_lib}"/*) install_name_tool -change "${dep}" "@loader_path/$(basename "${dep}")" "${f}" || exit 1 ;;
            esac
        done || return 1
        codesign --force --sign - "${f}" 2>/dev/null || return 1
    done
    log_line INFO "Rewrote install names under ${shared_dir} to @rpath/@loader_path"
}

collect_artifacts() {
    stage_dir="$1"
    platform_name="$2"
    arch_name="$3"
    config_line="$4"

    out_base="${RELEASE_DIR}/${platform_name}/${arch_name}/${VERSION}"
    out_shared="${out_base}/shared"
    out_include="${out_base}/include"
    mkdir -p "${out_shared}" "${out_include}"

    for libdir in lib lib64; do
        d="${stage_dir}/${libdir}"
        [ -d "${d}" ] || continue
        # -type f -o -type l (not just -f): libssl.so/libcrypto.so are symlinks to the
        # versioned .so.N -- skipping symlinks here would leave CMake's find_package(OpenSSL)
        # unable to find the unversioned name to link against. cp -P copies them as symlinks
        # (both link and target land in the same output dir, so the relative link still resolves).
        find "${d}" -maxdepth 1 \( -type f -o -type l \) \( -name '*.so' -o -name '*.so.*' -o -name '*.dylib' -o -name '*.dll' -o -name '*.dll.a' -o -name '*.lib' \) | while IFS= read -r f; do
            cp -Pf "${f}" "${out_shared}/"
        done
    done
    # MinGW/MSVC builds place the .dll itself next to the binaries, not in lib/.
    find "${stage_dir}/bin" -maxdepth 1 -type f -name '*.dll' 2>/dev/null | while IFS= read -r f; do
        cp -f "${f}" "${out_shared}/" 2>/dev/null || true
    done

    if [ -d "${stage_dir}/include" ]; then
        cp -R "${stage_dir}/include/." "${out_include}/"
    fi

    if [ "${platform_name}" = "mac" ]; then
        relocate_macos_dylibs "${out_shared}" "${stage_dir}/lib" || {
            log_line ERROR "Could not rewrite the dylib install names under ${out_shared}."
            return 1
        }
    fi

    {
        echo "timestamp=${TIMESTAMP}"
        echo "platform=${platform_name}"
        echo "arch=${arch_name}"
        echo "version=${VERSION}"
        echo "openssl_configure_target=${config_line}"
        echo "git_commit=$(git -C "${ROOT_DIR}" rev-parse --short HEAD 2>/dev/null || echo unknown)"
        echo "log_file=${LOG_FILE}"
    } > "${out_base}/build-info.txt"

    log_line INFO "Artifacts saved to ${out_base}"
}

build_one() {
    platform_name="$1"
    arch_name="$2"
    configure_target="$3"
    extra_args="$4"

    build_dir="${BUILD_ROOT}/${platform_name}/${arch_name}"
    stage_dir="${STAGE_ROOT}/${platform_name}-${arch_name}"
    rm -rf "${build_dir}" "${stage_dir}"
    mkdir -p "${build_dir}" "${stage_dir}"

    log_line INFO "Configuring ${platform_name}/${arch_name} (target: ${configure_target})"
    # shellcheck disable=SC2086
    if ! run_and_log_in "${build_dir}" perl "${ROOT_DIR}/Configure" ${configure_target} shared no-tests no-docs \
            --prefix="${stage_dir}" --openssldir="${stage_dir}/ssl" ${extra_args}; then
        log_line ERROR "Configure failed for ${platform_name}/${arch_name}."
        return 1
    fi

    log_line INFO "Building ${platform_name}/${arch_name} (jobs=${JOBS})"
    if ! run_and_log_in "${build_dir}" make -j "${JOBS}"; then
        log_line ERROR "Build failed for ${platform_name}/${arch_name}."
        return 1
    fi

    log_line INFO "Installing libs+headers for ${platform_name}/${arch_name}"
    if ! run_and_log_in "${build_dir}" make install_sw; then
        log_line ERROR "install_sw failed for ${platform_name}/${arch_name}."
        return 1
    fi

    collect_artifacts "${stage_dir}" "${platform_name}" "${arch_name}" "${configure_target} ${extra_args}"
}

build_mac() {
    log_line INFO "Starting mac/arm64 build"
    build_one mac arm64 "darwin64-arm64" ""
}

build_ios() {
    log_line INFO "Starting ios/arm64 build"
    cross_top="${IOS_CROSS_TOP:-}"
    cross_sdk="${IOS_CROSS_SDK:-}"
    if [ -z "${cross_top}" ] || [ -z "${cross_sdk}" ]; then
        if ! command -v xcrun >/dev/null 2>&1; then
            log_line ERROR "IOS_CROSS_TOP/IOS_CROSS_SDK are not set and xcrun is unavailable to auto-detect them (needs a macOS host with Xcode)."
            return 1
        fi
        sdk_path=$(xcrun --sdk iphoneos --show-sdk-path 2>/dev/null) || {
            log_line ERROR "xcrun could not locate the iphoneos SDK. Install Xcode's command line tools, or set IOS_CROSS_TOP/IOS_CROSS_SDK explicitly."
            return 1
        }
        cross_sdk=$(basename "${sdk_path}")
        cross_top=$(dirname "$(dirname "${sdk_path}")")
        log_line INFO "Auto-detected IOS_CROSS_TOP=${cross_top} IOS_CROSS_SDK=${cross_sdk} via xcrun"
    fi
    CROSS_TOP="${cross_top}" CROSS_SDK="${cross_sdk}" \
        build_one ios arm64 "ios64-cross" ""
}

build_android() {
    log_line INFO "Starting android/arm64 build"
    if [ -z "${ANDROID_NDK_HOME:-}" ]; then
        log_line ERROR "ANDROID_NDK_HOME is not set. Export ANDROID_NDK_HOME and retry."
        return 1
    fi
    android_api=${ANDROID_API:-24}
    host_tag=""
    case "$(uname -s)" in
        Linux) host_tag="linux-x86_64" ;;
        Darwin) host_tag="darwin-x86_64" ;;
        MINGW*|MSYS*|CYGWIN*) host_tag="windows-x86_64" ;;
        *) log_line ERROR "Cannot pick an Android NDK toolchain host tag for host OS $(uname -s)."; return 1 ;;
    esac
    ndk_bin="${ANDROID_NDK_HOME}/toolchains/llvm/prebuilt/${host_tag}/bin"
    if [ ! -d "${ndk_bin}" ]; then
        log_line ERROR "Android NDK toolchain bin dir not found: ${ndk_bin}"
        return 1
    fi
    ANDROID_NDK_ROOT="${ANDROID_NDK_HOME}" PATH="${ndk_bin}:${PATH}" \
        build_one android arm64 "android-arm64" "-D__ANDROID_API__=${android_api}"
}

build_linux() {
    [ "${ARCH}" = "arm64" ] && { build_linux_arm64; return $?; }
    log_line INFO "Starting linux/x64 build"
    extra=""
    if [ -n "${LINUX_X64_CROSS_PREFIX:-}" ]; then
        extra="--cross-compile-prefix=${LINUX_X64_CROSS_PREFIX}"
    elif [ "$(uname -s)" != "Linux" ]; then
        log_line ERROR "linux/x64 build on a non-Linux host needs LINUX_X64_CROSS_PREFIX."
        return 1
    fi
    build_one linux x64 "linux-x86_64" "${extra}"
}

# linux/arm64: native on an aarch64 Linux host, otherwise cross with the GNU aarch64
# toolchain (apt: gcc-aarch64-linux-gnu). OpenSSL's linux-aarch64 target carries its own
# armv8 asm, so nothing else changes.
build_linux_arm64() {
    log_line INFO "Starting linux/arm64 build"
    extra=""
    if [ "$(uname -s)" != "Linux" ] || [ "${HOST_ARCH}" != "arm64" ] || [ -n "${LINUX_ARM64_CROSS_PREFIX:-}" ]; then
        prefix="${LINUX_ARM64_CROSS_PREFIX:-aarch64-linux-gnu-}"
        if ! command -v "${prefix}gcc" >/dev/null 2>&1; then
            log_line ERROR "${prefix}gcc not found. Install gcc-aarch64-linux-gnu (or set LINUX_ARM64_CROSS_PREFIX)."
            return 1
        fi
        extra="--cross-compile-prefix=${prefix}"
    fi
    build_one linux arm64 "linux-aarch64" "${extra}"
}

# Locates the Visual Studio install root for the native-Windows MSVC build below. Same
# rationale/mechanism as deps/dolphin/scripts/build_dolphin_rvz.sh's
# find_vs_root(): mariadb-server links this OpenSSL build directly via explicit .lib paths
# (see deps/mariadb-server/scripts/build_mariadb_server.sh's ssl_defs_for), and an
# MSVC-linked mariadbd cannot consume a MinGW-built libcrypto/libssl (.dll.a import libs,
# MSYS2/UCRT-flavored headers) -- so this build targets MSVC on native Windows to match,
# same as dolphin/dolphinrvz already does (chdman-simd is the exception that stays MinGW).
find_vs_root() {
    vs_root="${VS_INSTALL_DIR:-C:\\Visual Studio\\18\\Community}"

    if [ ! -f "$(cygpath -u "${vs_root}\\VC\\Auxiliary\\Build\\vcvarsall.bat")" ]; then
        vswhere="/c/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe"
        if [ -f "${vswhere}" ]; then
            found_root=$("${vswhere}" -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>/dev/null | tr -d '\r')
            [ -n "${found_root}" ] && vs_root="${found_root}"
        fi
    fi

    if [ ! -f "$(cygpath -u "${vs_root}\\VC\\Auxiliary\\Build\\vcvarsall.bat")" ]; then
        log_line ERROR "Visual Studio install not found. Set VS_INSTALL_DIR to your install root, e.g. VS_INSTALL_DIR='C:\\Visual Studio\\18\\Community'"
        return 1
    fi

    printf '%s' "${vs_root}"
}

# Runs a small vcvars-initialized batch (one `call vcvarsall.bat` + the given command lines,
# each guarded by `if errorlevel 1 exit /b 1` -- including the last, which matters: a
# single-command sequence with no check after it, an earlier bug here, let a failing Configure
# silently report success) as a single cmd.exe child, capturing output the same way
# run_and_log_in does (temp file, then cat | tee -a LOG_FILE, so LOG_FILE always gets the real
# output regardless of how this function's own return value is used). $1 = human label for the
# [INFO] line; $2.. = command lines to run in order.
#
# TMP/TEMP are explicitly overridden to msvc_batch_tmp (an ordinary disk-backed directory under
# this build's own tree), not left at whatever the calling shell's ambient TMP/TEMP already is
# -- PATH is the only thing reset above; everything else in the environment, including TMP/
# TEMP, otherwise passes straight through to this batch. This turned out to matter a great
# deal: cl.exe writes scratch files during compilation to %TMP%/%TEMP%, and with those left
# pointed at a RAM-disk-backed drive (as this harness's own scratchpad happens to be), the
# very first compile of a from-scratch build reliably crashed with "cl : Command line error
# D8050: cannot execute '...\c1.dll': failed to get command line into debug records" --
# confirmed by extensive direct testing to disappear completely once TMP/TEMP point at a
# normal disk directory instead (a dozen-plus reproductions with it unset, zero failures with
# it set). If this build ever needs to run against some OTHER unusual TMP/TEMP setup and hits
# a similar toolchain crash again, this is the first thing to check.
run_vcvars_batch() {
    label="$1"; shift
    tmp_bat=$(mktemp --suffix=.bat)
    win_tmp_bat=$(cygpath -w "${tmp_bat}")
    {
        echo "@echo off"
        echo "set \"PATH=${msvc_batch_path}\""
        echo "set \"TMP=${msvc_batch_tmp}\""
        echo "set \"TEMP=${msvc_batch_tmp}\""
        echo "call \"${vcvarsall}\" ${vcvars_arch} >nul 2>&1"
        echo "cd /d \"${win_build_dir}\""
        for cmdline in "$@"; do
            echo "${cmdline}"
            echo "if errorlevel 1 exit /b 1"
        done
        echo "exit /b 0"
    } > "${tmp_bat}"

    log_line INFO "${label}"
    rc=0
    tmp_log="${LOG_DIR}/.cmd-$$-$(date +%s).log"
    MSYS2_ARG_CONV_EXCL="/c" cmd.exe /c "${win_tmp_bat}" < /dev/null > "${tmp_log}" 2>&1 || rc=$?
    cat "${tmp_log}" | tee -a "${LOG_FILE}"
    rm -f "${tmp_log}" "${tmp_bat}"
    return "${rc}"
}

# Configure and build+install run as two separate cmd.exe/vcvars sessions (each calling
# vcvarsall fresh), unlike dolphin's single-session CMake configure+build -- harmless either
# way here since OpenSSL's build doesn't need vcvars state to survive between Configure and
# nmake.
# OpenSSL's Configure needs a NATIVE Windows perl (one whose own paths use backslashes) for
# MSVC targets -- confirmed directly: it explicitly detects and refuses a Cygwin/MSYS-flavored
# perl ("This perl implementation doesn't produce Windows like paths"), which is exactly what
# a bare `perl` resolves to by default in an ordinary MSYS2 or Git-for-Windows shell (both ship
# their own cygwin-flavored perl ahead of any native one on PATH) -- so this can't just trust
# whatever `command -v perl` finds. `perl -e 'print $^O'` reports "MSWin32" for a native
# build and "cygwin" for the wrong kind; if PATH's perl is the wrong kind (or missing), fall
# back to Strawberry Perl's standard install location, same fallback pattern as find_vs_root.
find_native_perl() {
    candidate=$(command -v perl 2>/dev/null) || candidate=""
    if [ -n "${candidate}" ]; then
        os_name=$("${candidate}" -e 'print $^O' 2>/dev/null || true)
        [ "${os_name}" = "MSWin32" ] && { printf '%s' "${candidate}"; return 0; }
    fi
    for fallback in "${NATIVE_PERL:-}" "/c/Strawberry/perl/bin/perl.exe"; do
        [ -n "${fallback}" ] && [ -x "${fallback}" ] && { printf '%s' "${fallback}"; return 0; }
    done
    if [ -n "${candidate}" ]; then
        log_line ERROR "perl on PATH (${candidate}) is a Cygwin/MSYS build; OpenSSL's Configure refuses that for MSVC targets."
    else
        log_line ERROR "No perl found on PATH."
    fi
    log_line ERROR "Install a native Windows perl -- Strawberry Perl is the standard one: winget install StrawberryPerl.StrawberryPerl -- or set NATIVE_PERL to its perl.exe path."
    return 1
}

build_windows_msvc() {
    vs_root=$(find_vs_root) || return 1
    vcvarsall="${vs_root}\\VC\\Auxiliary\\Build\\vcvarsall.bat"

    # Resolved to a concrete directory (not left to bare `perl` on PATH) because the batch file
    # below starts from a clean, minimal PATH -- see dolphin's build script for why a clean
    # PATH matters for CMake-based builds; less critical for this Configure/nmake-based one,
    # but resolving it explicitly costs nothing and avoids depending on load order.
    native_perl=$(find_native_perl) || return 1
    perl_dir=$(cygpath -w "$(dirname "${native_perl}")")

    # Optional: OpenSSL's Configure auto-detects nasm for the accelerated x86_64 asm paths;
    # without it, it degrades to portable C. This repo already has nasm installed system-wide
    # for other submodules' builds, so use it when present rather than silently going without.
    nasm_seg=""
    [ "${ARCH}" = "x64" ] && [ -f "/c/nasm/nasm.exe" ] && nasm_seg=";C:\\nasm"

    # arm64 is cross-compiled from an x64 host (vcvarsall x64_arm64) or native on an arm64
    # one; VC-WIN64-ARM is OpenSSL's MSVC arm64 target and needs no assembler.
    case "${ARCH}" in
        x64) configure_target="VC-WIN64A"; vcvars_arch="x64" ;;
        arm64)
            configure_target="VC-WIN64-ARM"
            if [ "${PROCESSOR_ARCHITECTURE:-}" = "ARM64" ]; then vcvars_arch="arm64"; else vcvars_arch="x64_arm64"; fi ;;
    esac

    build_dir="${BUILD_ROOT}/windows/${ARCH}"
    stage_dir="${STAGE_ROOT}/windows-${ARCH}"
    wintemp_dir="${BUILD_ROOT}/_wintemp"
    rm -rf "${build_dir}" "${stage_dir}"
    mkdir -p "${build_dir}" "${stage_dir}" "${wintemp_dir}"

    win_root_dir=$(cygpath -w "${ROOT_DIR}")
    win_build_dir=$(cygpath -w "${build_dir}")
    win_stage_dir=$(cygpath -w "${stage_dir}")
    # cl.exe writes scratch files during compilation to %TMP%/%TEMP%, which this shell's own
    # ambient TMP/TEMP normally leaves pointed at this harness's own scratchpad drive -- not
    # touched by the PATH override above, since only PATH gets reset, everything else in the
    # environment still passes through to the batch below. Overriding it to an ordinary
    # disk-backed directory here, rather than trusting whatever TMP/TEMP already is, avoids
    # depending on that detail of the calling environment.
    msvc_batch_tmp=$(cygpath -w "${wintemp_dir}")
    msvc_batch_path="C:\\Windows\\System32;C:\\Windows;C:\\Windows\\System32\\Wbem;C:\\Windows\\System32\\WindowsPowerShell\\v1.0;${perl_dir}${nasm_seg}"

    log_line INFO "Using MSVC via: ${vcvarsall}"

    # MSYS2_ARG_CONV_EXCL="/c" (inside run_vcvars_batch): without it, MSYS2 mangles the literal
    # "/c" token (its own drive-mount notation) before cmd.exe ever sees it. < /dev/null:
    # cmd.exe launched under mintty otherwise gets no properly connected stdin and can hang.
    # Both confirmed by direct testing in dolphin's build script.
    if ! run_vcvars_batch "Configuring windows/${ARCH} (target: ${configure_target})" \
        "perl \"${win_root_dir}\\Configure\" ${configure_target} shared no-tests no-docs --prefix=\"${win_stage_dir}\" --openssldir=\"${win_stage_dir}\\ssl\""
    then
        log_line ERROR "Windows MSVC Configure failed (exit ${rc})."
        return "${rc}"
    fi

    if ! run_vcvars_batch "Building + installing windows/${ARCH}" "nmake" "nmake install_sw"; then
        log_line ERROR "Windows MSVC build/install failed (exit ${rc})."
        return "${rc}"
    fi

    collect_artifacts "${stage_dir}" "windows" "${ARCH}" "${configure_target}"
}

build_windows() {
    log_line INFO "Starting windows/${ARCH} build"
    case "$(uname -s)" in
        MINGW*|MSYS*|CYGWIN*)
            log_line INFO "Native Windows host detected; building with MSVC (VC-WIN64A) via vcvarsall, matching this repo's dolphin/dolphinrvz build."
            build_windows_msvc
            return $?
            ;;
    esac
    if [ "${ARCH}" = "arm64" ]; then
        log_line ERROR "windows/arm64 is built with MSVC on a Windows host only; cross-building it with mingw is not wired up."
        return 1
    fi
    cross_prefix="${WINDOWS_CROSS_PREFIX:-x86_64-w64-mingw32-}"
    if ! command -v "${cross_prefix}gcc" >/dev/null 2>&1; then
        log_line ERROR "${cross_prefix}gcc was not found on PATH. Install a mingw-w64 cross toolchain, or set WINDOWS_CROSS_PREFIX."
        return 1
    fi
    build_one windows x64 "mingw64" "--cross-compile-prefix=${cross_prefix}"
}

failures=""
case "${PLATFORM}" in
    mac) build_mac || failures="${failures} mac/arm64" ;;
    ios) build_ios || failures="${failures} ios/arm64" ;;
    android) build_android || failures="${failures} android/arm64" ;;
    linux) build_linux || failures="${failures} linux/${ARCH}" ;;
    windows) build_windows || failures="${failures} windows/${ARCH}" ;;
esac

if [ -n "${failures}" ]; then
    log_line ERROR "Build completed with failures:${failures}"
    log_line ERROR "See full details in ${LOG_FILE}"
    exit 1
fi

log_line INFO "Build completed successfully for: ${PLATFORM}/${ARCH}"
log_line INFO "Release root: ${RELEASE_DIR}"
log_line INFO "Log file: ${LOG_FILE}"
