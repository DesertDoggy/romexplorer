#!/bin/sh
# Builds libmariadb (MariaDB Connector/C) as a shared library, out-of-tree, for one of
# mac/ios/android/linux/windows. Mirrors the build-script convention used by the other
# submodules in this repo (see e.g. deps/zlib-ng/scripts/build-zlib-ng-release.sh):
# nothing is written back into the submodule's own tree, everything lands under
# deps/mariadb-connector-c/release/<platform>/<arch>/<version>/{shared,include}.
#
# No source patch is needed: every option this script sets (WITH_UNIT_TESTS, WITH_CURL,
# WITH_MYSQLCOMPAT, WITH_SSL, ...) is a first-class CMake cache option this project already
# exposes.
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
USER_DIR=$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)
REPO_ROOT=$(CDPATH= cd -- "${USER_DIR}/../.." && pwd)
# The upstream source tree (a submodule). This script, its patches and its build output
# (release/, _build/, logs/) live in deps/mariadb-connector-c/ instead.
ROOT_DIR="${MARIADB_CONNECTOR_C_SOURCE_DIR:-${REPO_ROOT}/external/mariadb-connector-c}"
RELEASE_DIR="${USER_DIR}/release"
BUILD_ROOT="${USER_DIR}/_build"
STAGE_ROOT="${BUILD_ROOT}/_stage"
LOG_DIR="${USER_DIR}/logs"

PLATFORM=""
ARCH=""
PLATFORM_SET=0
CLEAN=1
VERSION_OVERRIDE=""
WITH_SSL_MODE="auto"

TIMESTAMP=$(date +%Y%m%d-%H%M%S)
LOG_FILE="${LOG_DIR}/build-connector-c-${TIMESTAMP}.log"

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

usage() {
    cat << 'EOF'
Usage:
  sh deps/mariadb-connector-c/scripts/build_connector_c.sh [options]

Options:
  --platform <mac|ios|android|linux|windows>
  --arch <x64|arm64>   linux/windows only (default: the host's); mac/ios/android are arm64
  --clean | --no-clean
  --version <value>
  --with-ssl <auto|on|off>   (default: auto)
  --help

Environment variables:
  IOS_TOOLCHAIN_FILE        Required for --platform ios
  IOS_SYSROOT               Optional for iOS (default: iphoneos)
  ANDROID_NDK_HOME          Required for --platform android
  ANDROID_PLATFORM          Optional for Android (default: android-24)
  WINDOWS_TOOLCHAIN_FILE    Required for --platform windows on a non-Windows host
  LINUX_X64_TOOLCHAIN_FILE  Optional for a Linux x64 cross build
  LINUX_X64_CC              Optional x86_64 Linux C compiler path/name
  LINUX_ARM64_TOOLCHAIN_FILE Optional for linux/arm64 from a non-arm64 host
  LINUX_ARM64_CROSS_PREFIX  Otherwise the GNU cross prefix used for it (default:
                            aarch64-linux-gnu-, apt: gcc-aarch64-linux-gnu g++-aarch64-linux-gnu)
  OPENSSL_ROOT_DIR          Required on linux/mac/ios/android (see --with-ssl below)
  JOBS                      Optional build parallelism (default: host CPU count)

--with-ssl: MariaDB Connector/C has no "build without TLS" option -- WITH_SSL must resolve
to OpenSSL, GnuTLS, or (Windows-only) Schannel. This build never falls back to a system-
installed OpenSSL/GnuTLS package (so the same command behaves the same on Windows, which has
neither) -- windows uses Schannel (built into the OS, no dependency); linux/mac/ios/android
require OPENSSL_ROOT_DIR pointing at this repo's own OpenSSL build
(deps/openssl/release/<platform>/<arch>/<version>/), i.e. build that submodule first.
  auto (default): schannel on windows, OPENSSL_ROOT_DIR-based OpenSSL elsewhere
  on:             force OpenSSL; OPENSSL_ROOT_DIR is required
  off:            not supported by connector-c's own CMakeLists -- kept only so an explicit
                  request fails fast with this explanation instead of a raw CMake error
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
        --with-ssl)
            [ "$#" -ge 2 ] || { log_line ERROR "Missing value for --with-ssl"; exit 2; }
            WITH_SSL_MODE="$2"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) log_line ERROR "Unknown argument: $1"; usage; exit 2 ;;
    esac
done

case "${WITH_SSL_MODE}" in
    auto|on|off) ;;
    *) log_line ERROR "Invalid --with-ssl value: ${WITH_SSL_MODE}"; exit 2 ;;
esac

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

if ! command -v cmake >/dev/null 2>&1; then
    log_line ERROR "cmake was not found. Install cmake and retry."
    exit 2
fi

if [ -n "${VERSION_OVERRIDE}" ]; then
    VERSION="${VERSION_OVERRIDE}"
    log_line INFO "Using version override: ${VERSION}"
else
    VERSION=$(
        awk '
            /CPACK_PACKAGE_VERSION_MAJOR/ { gsub(/[^0-9]/, "", $0); maj=$0 }
            /CPACK_PACKAGE_VERSION_MINOR/ { gsub(/[^0-9]/, "", $0); min=$0 }
            /CPACK_PACKAGE_VERSION_PATCH/ { gsub(/[^0-9]/, "", $0); pat=$0 }
            END { if (maj != "" && min != "" && pat != "") print maj "." min "." pat }
        ' "${ROOT_DIR}/CMakeLists.txt"
    )
    if [ -z "${VERSION}" ]; then
        VERSION=$(git -C "${ROOT_DIR}" describe --tags 2>/dev/null || date +%Y%m%d-%H%M%S)
        log_line FALLBACK "Could not read CPACK_PACKAGE_VERSION_* from CMakeLists.txt. Using ${VERSION}."
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

# CMAKE_COMPILE_WARNING_AS_ERROR=OFF: connector-c's own CMakeLists only adds -Werror when
# this variable is left undefined (see its CMakeLists.txt around WARNING_AS_ERROR) -- setting
# it, rather than patching source, is its own documented escape hatch. This matters because a
# newer OpenSSL than connector-c's secure/openssl.c was written against (e.g. this repo's own
# vendored deps/openssl, which deprecates X509_check_host/X509_check_ip_asc) turns a warning
# into a hard error otherwise.
COMMON_DEFS="-DCMAKE_BUILD_TYPE=Release -DCMAKE_COMPILE_WARNING_AS_ERROR=OFF -DWITH_UNIT_TESTS=OFF -DWITH_CURL=OFF -DWITH_EXTERNAL_ZLIB=OFF -DWITH_DYNCOL=ON"

# Auto-discovers the most recently built deps/openssl output for $1=platform
# $2=arch, i.e. the newest mtime dir under deps/openssl/release/<platform>/<arch>/*/.
# Prints the path on stdout, or nothing (with exit 1) if openssl hasn't been built for that
# platform/arch yet. "Latest" is by build time, not by parsing the version string as semver
# -- version strings here (git describe output, or a submodule's own X.Y.Z) aren't uniformly
# sortable, but "most recently built" is exactly what a caller who didn't pass an explicit
# OPENSSL_ROOT_DIR wants.
auto_discover_openssl_root() {
    platform_name="$1"
    arch_name="$2"
    candidates_dir="${REPO_ROOT}/deps/openssl/release/${platform_name}/${arch_name}"
    [ -d "${candidates_dir}" ] || return 1
    latest=$(ls -1dt "${candidates_dir}"/*/ 2>/dev/null | head -n 1)
    [ -n "${latest}" ] || return 1
    printf '%s' "${latest%/}"
}

ssl_defs_for() {
    # $1 = platform, $2 = arch. Prints CMake defs on stdout; logs/errors to stderr (this is
    # called as `x=$(ssl_defs_for ...)`, so stdout must carry only the defs). Never touches a
    # system-installed OpenSSL/GnuTLS -- see usage()'s --with-ssl section for why.
    platform_name="$1"
    arch_name="$2"

    if [ "${WITH_SSL_MODE}" = "off" ]; then
        log_line ERROR "--with-ssl off is not supported: MariaDB Connector/C's own CMakeLists requires WITH_SSL to resolve to OpenSSL, GnuTLS, or Schannel -- there is no 'no TLS' build." >&2
        return 1
    fi

    if [ "${platform_name}" = "windows" ] && [ "${WITH_SSL_MODE}" != "on" ] && [ -z "${OPENSSL_ROOT_DIR:-}" ]; then
        printf '%s' "-DWITH_SSL=ON"
        return 0
    fi

    if [ -z "${OPENSSL_ROOT_DIR:-}" ]; then
        auto_root=$(auto_discover_openssl_root "${platform_name}" "${arch_name}") && [ -n "${auto_root}" ] || auto_root=""
        if [ -n "${auto_root}" ]; then
            OPENSSL_ROOT_DIR="${auto_root}"
            log_line INFO "OPENSSL_ROOT_DIR not set; auto-discovered latest build: ${OPENSSL_ROOT_DIR}" >&2
        fi
    fi

    if [ -z "${OPENSSL_ROOT_DIR:-}" ]; then
        log_line ERROR "OPENSSL_ROOT_DIR is not set for ${platform_name}, and no build was found under deps/openssl/release/${platform_name}/${arch_name}/. Build the vendored OpenSSL submodule first (deps/openssl/scripts/build_openssl.sh --platform ${platform_name}) or pass an existing output dir (.../${platform_name}/<arch>/<version>) as OPENSSL_ROOT_DIR." >&2
        return 1
    fi

    # OPENSSL_ROOT_DIR alone only helps CMake's FindOpenSSL when the dir has the
    # lib(64)/+include/ layout it searches for. Our own openssl build script publishes
    # shared/+include/ instead (see build_openssl.sh), so resolve the exact library/header
    # paths ourselves and pass them directly -- this also works unmodified if the caller
    # points OPENSSL_ROOT_DIR at a conventional lib/include install instead.
    inc_dir="${OPENSSL_ROOT_DIR}/include"
    lib_dir=""
    for cand in "${OPENSSL_ROOT_DIR}/shared" "${OPENSSL_ROOT_DIR}/lib" "${OPENSSL_ROOT_DIR}/lib64"; do
        [ -d "${cand}" ] && { lib_dir="${cand}"; break; }
    done
    if [ ! -d "${inc_dir}" ] || [ -z "${lib_dir}" ]; then
        log_line ERROR "OPENSSL_ROOT_DIR=${OPENSSL_ROOT_DIR} is missing include/ or a shared|lib|lib64 dir. Point it at a build_openssl.sh output dir (.../${platform_name}/<arch>/<version>)." >&2
        return 1
    fi
    crypto_lib=$(find "${lib_dir}" -maxdepth 1 \( -type f -o -type l \) \( -name 'libcrypto.so' -o -name 'libcrypto.dylib' -o -name 'libcrypto.dll.a' -o -name 'libcrypto.lib' \) | head -n 1)
    ssl_lib=$(find "${lib_dir}" -maxdepth 1 \( -type f -o -type l \) \( -name 'libssl.so' -o -name 'libssl.dylib' -o -name 'libssl.dll.a' -o -name 'libssl.lib' \) | head -n 1)
    if [ -z "${crypto_lib}" ] || [ -z "${ssl_lib}" ]; then
        log_line ERROR "Could not find libcrypto/libssl under ${lib_dir}." >&2
        return 1
    fi
    log_line INFO "Using OpenSSL: include=${inc_dir} crypto=${crypto_lib} ssl=${ssl_lib}" >&2
    printf '%s' "-DWITH_SSL=ON -DOPENSSL_ROOT_DIR=${OPENSSL_ROOT_DIR} -DOPENSSL_INCLUDE_DIR=${inc_dir} -DOPENSSL_CRYPTO_LIBRARY=${crypto_lib} -DOPENSSL_SSL_LIBRARY=${ssl_lib}"
}

non_windows_defs() {
    case "$1" in
        windows) printf '%s' "" ;;
        *) printf '%s' "-DWITH_MYSQLCOMPAT=OFF -DWITH_DOCS=OFF" ;;
    esac
}

rpath_defs_for() {
    # Makes libmariadb.so/.dylib look for libssl/libcrypto next to itself (same "shared"
    # dir), instead of requiring callers to set LD_LIBRARY_PATH/DYLD_LIBRARY_PATH -- our own
    # OpenSSL build has no fixed system install location to fall back to.
    case "$1" in
        mac|ios) printf '%s' "-DCMAKE_INSTALL_RPATH=@loader_path" ;;
        linux|android) printf '%s' "-DCMAKE_INSTALL_RPATH=\$ORIGIN" ;;
        *) printf '%s' "" ;;
    esac
}

# mac only: CMake searches /opt/homebrew (and /usr/local) by default, so whichever of
# zstd/lz4/... the build machine happens to have from Homebrew gets linked by absolute path
# -- seen as zstd.so -> /opt/homebrew/opt/zstd/lib/libzstd.1.dylib and provider_lz4.so ->
# .../liblz4.1.dylib. Those plugins then exist or not depending on the machine, and fail to
# load anywhere else. Ignoring the prefixes makes the build see only the SDK and what this
# script hands it explicitly (OpenSSL). Tools found through PATH (bison) are unaffected.
homebrew_isolation_defs_for() {
    case "$1" in
        mac) printf '%s' "-DCMAKE_IGNORE_PREFIX_PATH=/opt/homebrew;/usr/local" ;;
        *) printf '%s' "" ;;
    esac
}

collect_artifacts() {
    build_dir="$1"
    platform_name="$2"
    arch_name="$3"
    all_defs="$4"

    stage_dir="${STAGE_ROOT}/${platform_name}-${arch_name}"
    rm -rf "${stage_dir}"
    mkdir -p "${stage_dir}"

    if ! run_and_log cmake --install "${build_dir}" --prefix "${stage_dir}"; then
        log_line ERROR "cmake --install failed for ${platform_name}/${arch_name}"
        return 1
    fi

    out_base="${RELEASE_DIR}/${platform_name}/${arch_name}/${VERSION}"
    out_shared="${out_base}/shared"
    out_include="${out_base}/include"
    mkdir -p "${out_shared}" "${out_include}"

    stage_lib_dir="${stage_dir}/lib"
    [ -d "${stage_lib_dir}" ] || stage_lib_dir="${stage_dir}/lib64"
    if [ -d "${stage_lib_dir}" ]; then
        # -type f -o -type l: the unversioned libmariadb.so is a symlink to libmariadb.so.3;
        # skipping symlinks would drop the name callers actually link against. cp -P keeps it
        # a symlink (link + target both land in out_shared, so it still resolves).
        find "${stage_lib_dir}" \( -type f -o -type l \) \( -name '*.so' -o -name '*.so.*' -o -name '*.dylib' -o -name '*.dll' -o -name '*.dll.a' -o -name '*.lib' \) | while IFS= read -r f; do
            cp -Pf "${f}" "${out_shared}/"
        done
    fi
    # Windows shared runtime commonly lands next to the binaries dir on some generators.
    find "${build_dir}" -maxdepth 3 -type f -name '*.dll' 2>/dev/null | while IFS= read -r f; do
        cp -f "${f}" "${out_shared}/" 2>/dev/null || true
    done

    if [ -d "${stage_dir}/include" ]; then
        cp -R "${stage_dir}/include/." "${out_include}/"
    fi

    {
        echo "timestamp=${TIMESTAMP}"
        echo "platform=${platform_name}"
        echo "arch=${arch_name}"
        echo "version=${VERSION}"
        echo "cmake=$(cmake --version | head -n 1)"
        echo "git_commit=$(git -C "${ROOT_DIR}" rev-parse --short HEAD 2>/dev/null || echo unknown)"
        echo "definitions=${all_defs}"
        echo "log_file=${LOG_FILE}"
    } > "${out_base}/build-info.txt"

    log_line INFO "Artifacts saved to ${out_base}"
}

build_one() {
    platform_name="$1"
    arch_name="$2"
    extra_defs="$3"

    build_dir="${BUILD_ROOT}/${platform_name}/${arch_name}"
    rm -rf "${build_dir}"
    mkdir -p "${build_dir}"

    ssl_defs=$(ssl_defs_for "${platform_name}" "${arch_name}") || return 1
    defs="${COMMON_DEFS} $(non_windows_defs "${platform_name}") $(rpath_defs_for "${platform_name}") $(homebrew_isolation_defs_for "${platform_name}") ${ssl_defs} ${extra_defs}"

    log_line INFO "Configuring ${platform_name}/${arch_name}"
    if ! run_and_log cmake -S "${ROOT_DIR}" -B "${build_dir}" ${defs}; then
        log_line ERROR "Configure failed for ${platform_name}/${arch_name}."
        return 1
    fi

    log_line INFO "Building ${platform_name}/${arch_name} (jobs=${JOBS})"
    # Build the default "all" target, not just libmariadb: cmake --install below runs every
    # configured install rule (including the auth/pvio plugins, e.g. dialog.so), so anything
    # left unbuilt makes the install step fail looking for a missing file.
    if ! run_and_log cmake --build "${build_dir}" --config Release -j "${JOBS}"; then
        log_line ERROR "Build failed for ${platform_name}/${arch_name}."
        return 1
    fi

    collect_artifacts "${build_dir}" "${platform_name}" "${arch_name}" "${defs}"
}

build_mac() {
    log_line INFO "Starting mac/arm64 build"
    build_one mac arm64 "-DCMAKE_OSX_ARCHITECTURES=arm64"
}

build_ios() {
    log_line INFO "Starting ios/arm64 build"
    if [ -z "${IOS_TOOLCHAIN_FILE:-}" ]; then
        log_line ERROR "IOS_TOOLCHAIN_FILE is not set. Export IOS_TOOLCHAIN_FILE and retry."
        return 1
    fi
    if [ ! -f "${IOS_TOOLCHAIN_FILE}" ]; then
        log_line ERROR "IOS_TOOLCHAIN_FILE does not exist: ${IOS_TOOLCHAIN_FILE}"
        return 1
    fi
    ios_sysroot=${IOS_SYSROOT:-iphoneos}
    build_one ios arm64 "-DCMAKE_TOOLCHAIN_FILE=${IOS_TOOLCHAIN_FILE} -DCMAKE_SYSTEM_NAME=iOS -DCMAKE_OSX_SYSROOT=${ios_sysroot} -DCMAKE_OSX_ARCHITECTURES=arm64"
}

build_android() {
    log_line INFO "Starting android/arm64 build"
    if [ -z "${ANDROID_NDK_HOME:-}" ]; then
        log_line ERROR "ANDROID_NDK_HOME is not set. Export ANDROID_NDK_HOME and retry."
        return 1
    fi
    ndk_toolchain="${ANDROID_NDK_HOME}/build/cmake/android.toolchain.cmake"
    if [ ! -f "${ndk_toolchain}" ]; then
        log_line ERROR "Android toolchain not found: ${ndk_toolchain}"
        return 1
    fi
    android_platform=${ANDROID_PLATFORM:-android-24}
    build_one android arm64 "-DCMAKE_TOOLCHAIN_FILE=${ndk_toolchain} -DANDROID_ABI=arm64-v8a -DANDROID_PLATFORM=${android_platform}"
}

build_linux() {
    [ "${ARCH}" = "arm64" ] && { build_linux_arm64; return $?; }
    log_line INFO "Starting linux/x64 build"
    uname_s=$(uname -s)
    extra=""
    if [ -n "${LINUX_X64_TOOLCHAIN_FILE:-}" ]; then
        [ -f "${LINUX_X64_TOOLCHAIN_FILE}" ] || { log_line ERROR "LINUX_X64_TOOLCHAIN_FILE does not exist: ${LINUX_X64_TOOLCHAIN_FILE}"; return 1; }
        extra="-DCMAKE_TOOLCHAIN_FILE=${LINUX_X64_TOOLCHAIN_FILE}"
    elif [ -n "${LINUX_X64_CC:-}" ]; then
        extra="-DCMAKE_SYSTEM_NAME=Linux -DCMAKE_C_COMPILER=${LINUX_X64_CC}"
    elif [ "${uname_s}" != "Linux" ]; then
        log_line ERROR "linux/x64 build on non-Linux host needs LINUX_X64_TOOLCHAIN_FILE or LINUX_X64_CC."
        return 1
    fi
    build_one linux x64 "${extra}"
}

# linux/arm64: native on an aarch64 Linux host; otherwise a toolchain file, or the GNU
# aarch64 cross compilers with the find root pointed at their sysroot, so nothing from the
# x86_64 host's /usr is picked up.
build_linux_arm64() {
    log_line INFO "Starting linux/arm64 build"
    extra=""
    if [ -n "${LINUX_ARM64_TOOLCHAIN_FILE:-}" ]; then
        [ -f "${LINUX_ARM64_TOOLCHAIN_FILE}" ] || { log_line ERROR "LINUX_ARM64_TOOLCHAIN_FILE does not exist: ${LINUX_ARM64_TOOLCHAIN_FILE}"; return 1; }
        extra="-DCMAKE_TOOLCHAIN_FILE=${LINUX_ARM64_TOOLCHAIN_FILE}"
    elif [ "$(uname -s)" != "Linux" ] || [ "${HOST_ARCH}" != "arm64" ] || [ -n "${LINUX_ARM64_CROSS_PREFIX:-}" ]; then
        prefix="${LINUX_ARM64_CROSS_PREFIX:-aarch64-linux-gnu-}"
        command -v "${prefix}gcc" >/dev/null 2>&1 || { log_line ERROR "${prefix}gcc not found. Install gcc-aarch64-linux-gnu (or set LINUX_ARM64_TOOLCHAIN_FILE)."; return 1; }
        sysroot="/usr/${prefix%-}"
        extra="-DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=aarch64 -DCMAKE_C_COMPILER=${prefix}gcc -DCMAKE_CXX_COMPILER=${prefix}g++ -DCMAKE_FIND_ROOT_PATH=${sysroot} -DCMAKE_FIND_ROOT_PATH_MODE_PROGRAM=NEVER -DCMAKE_FIND_ROOT_PATH_MODE_LIBRARY=ONLY -DCMAKE_FIND_ROOT_PATH_MODE_INCLUDE=ONLY -DCMAKE_FIND_ROOT_PATH_MODE_PACKAGE=ONLY"
    fi
    build_one linux arm64 "${extra}"
}

# Locates the Visual Studio install root for the native-Windows MSVC build below. Same
# mechanism as deps/dolphin/scripts/build_dolphin_rvz.sh's find_vs_root() and
# deps/openssl/scripts/build_openssl.sh's copy of it -- duplicated here rather than
# shared, matching this repo's existing convention of self-contained build scripts (no
# common/lib file; log_line/run_and_log are duplicated the same way across all of these).
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

# Rewrites any -Dkey=/posix/path token in a defs string to -Dkey=<native Windows path>, e.g.
# from ssl_defs_for's -DOPENSSL_INCLUDE_DIR=/d/.../include when --with-ssl on/OPENSSL_ROOT_DIR
# is used on Windows. Needed only for the MSVC batch-file path below: those defs get embedded
# as literal text in a .bat file run by the VS-bundled (non-MSYS) cmake.exe, which has no
# concept of MSYS2's /d/... drive-mount notation (unlike a bare argv bash hands an .exe
# directly, which MSYS2 auto-translates). Non-path tokens (flags, ON/OFF, etc) pass through
# unchanged.
winpathify_defs() {
    result=""
    for tok in $1; do
        case "${tok}" in
            *=/*)
                key="${tok%%=*}"
                val="${tok#*=}"
                val=$(cygpath -w "${val}" 2>/dev/null || printf '%s' "${val}")
                tok="${key}=${val}"
                ;;
        esac
        result="${result} ${tok}"
    done
    printf '%s' "${result# }"
}

# Configure+build run inside ONE batch file after `call vcvarsall.bat`, same
# confirmed-by-testing reason as dolphin's build script: bash-exported INCLUDE/LIB/LIBPATH
# stop reaching link.exe several process-hops down the bash -> cmake.exe -> ninja.exe ->
# cmd.exe -> link.exe chain. Running cmake as a direct child of the same vcvars-configured
# cmd.exe avoids that. PATH is reset to a clean, MSYS2-free base before calling vcvarsall.bat
# so CMake's find_package/find_library machinery can't pick up MinGW-targeted "system"
# zlib/openssl/etc reachable only via mingw64/bin's pkg-config.exe -- confirmed to actually
# happen (not just theoretical) when building dolphin/dolphinrvz this same way.
build_one_windows_msvc() {
    extra_defs="$1"
    platform_name="windows"
    arch_name="${ARCH}"
    # arm64 is cross-compiled from an x64 host (x64_arm64) or native on an arm64 one.
    case "${ARCH}" in
        x64) vcvars_arch="x64" ;;
        arm64) if [ "${PROCESSOR_ARCHITECTURE:-}" = "ARM64" ]; then vcvars_arch="arm64"; else vcvars_arch="x64_arm64"; fi ;;
    esac

    build_dir="${BUILD_ROOT}/${platform_name}/${arch_name}"
    wintemp_dir="${BUILD_ROOT}/_wintemp"
    rm -rf "${build_dir}"
    mkdir -p "${build_dir}" "${wintemp_dir}"

    ssl_defs=$(ssl_defs_for "${platform_name}" "${arch_name}") || return 1
    defs="${COMMON_DEFS} $(rpath_defs_for "${platform_name}") ${ssl_defs} ${extra_defs}"
    defs=$(winpathify_defs "${defs}")

    vs_root=$(find_vs_root) || return 1
    vcvarsall="${vs_root}\\VC\\Auxiliary\\Build\\vcvarsall.bat"
    vs_cmake_dir="${vs_root}\\Common7\\IDE\\CommonExtensions\\Microsoft\\CMake\\CMake\\bin"
    vs_ninja_dir="${vs_root}\\Common7\\IDE\\CommonExtensions\\Microsoft\\CMake\\Ninja"
    vs_cmake_exe="${vs_cmake_dir}\\cmake.exe"
    vs_ninja_exe="${vs_ninja_dir}\\ninja.exe"
    [ -f "$(cygpath -u "${vs_cmake_exe}")" ] || { log_line ERROR "VS-bundled cmake.exe not found: ${vs_cmake_exe} (needs the \"C++ CMake tools for Windows\" component)"; return 1; }
    [ -f "$(cygpath -u "${vs_ninja_exe}")" ] || { log_line ERROR "VS-bundled ninja.exe not found: ${vs_ninja_exe} (needs the \"C++ CMake tools for Windows\" component)"; return 1; }

    win_root_dir=$(cygpath -w "${ROOT_DIR}")
    win_build_dir=$(cygpath -w "${build_dir}")
    win_tmp_dir=$(cygpath -w "${wintemp_dir}")

    log_line INFO "Using MSVC via: ${vcvarsall}"
    log_line INFO "Configuring ${platform_name}/${arch_name} (MSVC/Ninja)"

    tmp_bat=$(mktemp --suffix=.bat)
    win_tmp_bat=$(cygpath -w "${tmp_bat}")
    {
        echo "@echo off"
        echo "set \"PATH=C:\\Windows\\System32;C:\\Windows;C:\\Windows\\System32\\Wbem;C:\\Windows\\System32\\WindowsPowerShell\\v1.0;C:\\Windows\\System32\\OpenSSH;${vs_cmake_dir};${vs_ninja_dir}\""
        # TMP/TEMP overridden to an ordinary disk-backed directory: cl.exe writes scratch
        # files there during compilation, and this call's own ambient TMP/TEMP -- unlike PATH,
        # not touched by the reset above -- can be pointed anywhere by the calling environment
        # (confirmed directly, on deps/openssl's build: a RAM-disk-backed TMP/TEMP reliably
        # crashed cl.exe's very first invocation of a from-scratch build with "Command line
        # error D8050: cannot execute '...\c1.dll': failed to get command line into debug
        # records", gone completely once TMP/TEMP pointed at a normal directory instead).
        echo "set \"TMP=${win_tmp_dir}\""
        echo "set \"TEMP=${win_tmp_dir}\""
        echo "call \"${vcvarsall}\" ${vcvars_arch} >nul 2>&1"
        echo "echo [INFO] Configuring ..."
        # shellcheck disable=SC2086
        echo "\"${vs_cmake_exe}\" -S \"${win_root_dir}\" -B \"${win_build_dir}\" -G Ninja -DCMAKE_MAKE_PROGRAM=\"${vs_ninja_exe}\" -DCMAKE_C_COMPILER=cl -DCMAKE_CXX_COMPILER=cl ${defs}"
        echo "if errorlevel 1 exit /b 1"
        echo "echo [INFO] Building ..."
        echo "\"${vs_cmake_exe}\" --build \"${win_build_dir}\" --config Release -j${JOBS}"
    } > "${tmp_bat}"

    rc=0
    tmp_log="${LOG_DIR}/.cmd-$$-$(date +%s).log"
    MSYS2_ARG_CONV_EXCL="/c" cmd.exe /c "${win_tmp_bat}" < /dev/null > "${tmp_log}" 2>&1 || rc=$?
    cat "${tmp_log}" | tee -a "${LOG_FILE}"
    rm -f "${tmp_log}" "${tmp_bat}"
    if [ "${rc}" -ne 0 ]; then
        log_line ERROR "Windows MSVC configure/build failed (exit ${rc})."
        return "${rc}"
    fi

    collect_artifacts "${build_dir}" "${platform_name}" "${arch_name}" "${defs}"
}

build_windows() {
    log_line INFO "Starting windows/${ARCH} build"
    case "$(uname -s)" in
        MINGW*|MSYS*|CYGWIN*)
            log_line INFO "Native Windows host detected; building with MSVC via vcvarsall, matching this repo's dolphin/dolphinrvz build."
            build_one_windows_msvc ""
            return $?
            ;;
    esac
    if [ "${ARCH}" = "arm64" ]; then
        log_line ERROR "windows/arm64 is built with MSVC on a Windows host only; cross-building it is not wired up."
        return 1
    fi
    if [ -z "${WINDOWS_TOOLCHAIN_FILE:-}" ]; then
        log_line ERROR "WINDOWS_TOOLCHAIN_FILE is not set (required to cross-compile windows/x64 from a non-Windows host)."
        return 1
    fi
    [ -f "${WINDOWS_TOOLCHAIN_FILE}" ] || { log_line ERROR "WINDOWS_TOOLCHAIN_FILE does not exist: ${WINDOWS_TOOLCHAIN_FILE}"; return 1; }
    build_one windows x64 "-DCMAKE_TOOLCHAIN_FILE=${WINDOWS_TOOLCHAIN_FILE}"
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
