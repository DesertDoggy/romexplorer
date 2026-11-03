#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname "$0")" && pwd)
USER_DIR=$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)
REPO_ROOT=$(CDPATH= cd -- "${USER_DIR}/../.." && pwd)
# The upstream source tree (a submodule). This script, its patches and its build output
# (release/, _build/, logs/) live in deps/zlib-ng/ instead.
ROOT_DIR="${ZLIBNG_SOURCE_DIR:-${REPO_ROOT}/external/zlib-ng}"
RELEASE_DIR="${USER_DIR}/release"
BUILD_ROOT="${RELEASE_DIR}/_build"
LOG_DIR="${USER_DIR}/logs"

PLATFORM=""
PLATFORM_SET=0
ARCH=""
CLEAN=1
VERSION_OVERRIDE=""
C_STANDARD_MODE="auto"
USED_C_STANDARD=""

TIMESTAMP=$(date +%Y%m%d-%H%M%S)
LOG_FILE="${LOG_DIR}/build-${TIMESTAMP}.log"

mkdir -p "${LOG_DIR}" "${RELEASE_DIR}" "${BUILD_ROOT}"
: > "${LOG_FILE}"

log_line() {
    level="$1"
    shift
    msg="$*"
    line="[${level}] ${msg}"
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

    if [ "${rc}" -eq 0 ]; then
        return 0
    fi

    log_line ERROR "Command failed (exit=${rc}): $*"
    return "${rc}"
}

usage() {
    cat << 'EOF'
Usage:
  sh deps/zlib-ng/scripts/build-zlib-ng-release.sh [options]

Options:
    --platform <mac|ios|android|linux|windows>
    --arch <x64|arm64>   linux/windows only (default: the host's); mac/ios/android are arm64
    --clean
    --no-clean
  --version <value>
  --c-standard <auto|23|11>
  --help

Environment variables:
  IOS_TOOLCHAIN_FILE        Required for --platform ios
  IOS_SYSROOT               Optional for iOS (default: iphoneos)
  ANDROID_NDK_HOME          Required for --platform android
  ANDROID_PLATFORM          Optional for Android (default: android-24)
  WINDOWS_TOOLCHAIN_FILE    Optional override for Windows x64 toolchain
  LINUX_X64_TOOLCHAIN_FILE  Optional for Linux x64 cross build
  LINUX_X64_CC              Optional x86_64 Linux C compiler path/name
  LINUX_ARM64_TOOLCHAIN_FILE Optional for linux/arm64 from a non-arm64 host (default:
                            cmake/toolchain-aarch64.cmake, needs gcc-aarch64-linux-gnu)
  WINDOWS_ARM64_TOOLCHAIN_FILE Optional for windows/arm64 (default:
                            cmake/toolchain-llvm-mingw-aarch64.cmake, needs llvm-mingw's
                            aarch64-w64-mingw32-clang on PATH)
  JOBS                      Optional build parallelism (default: host CPU count)
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --arch)
            [ "$#" -ge 2 ] || { log_line ERROR "Missing value for --arch"; exit 2; }
            ARCH="$2"
            shift 2
            ;;
        --platform)
            [ "$#" -ge 2 ] || { log_line ERROR "Missing value for --platform"; exit 2; }
            PLATFORM="$2"
            PLATFORM_SET=1
            shift 2
            ;;
        --clean)
            CLEAN=1
            shift
            ;;
        --no-clean)
            CLEAN=0
            shift
            ;;
        --version)
            [ "$#" -ge 2 ] || { log_line ERROR "Missing value for --version"; exit 2; }
            VERSION_OVERRIDE="$2"
            shift 2
            ;;
        --c-standard)
            [ "$#" -ge 2 ] || { log_line ERROR "Missing value for --c-standard"; exit 2; }
            C_STANDARD_MODE="$2"
            shift 2
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        *)
            log_line ERROR "Unknown argument: $1"
            usage
            exit 2
            ;;
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
        Darwin)
            PLATFORM="mac"
            ;;
        Linux)
            PLATFORM="linux"
            ;;
        MINGW*|MSYS*|CYGWIN*)
            PLATFORM="windows"
            ;;
        *)
            log_line ERROR "Unsupported host OS: ${host_os}. Use --platform to select a target explicitly."
            exit 2
            ;;
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

case "${C_STANDARD_MODE}" in
    auto|23|11) ;;
    *) log_line ERROR "Invalid --c-standard value: ${C_STANDARD_MODE}"; exit 2 ;;
esac

if ! command -v cmake >/dev/null 2>&1; then
    log_line ERROR "cmake was not found. Install cmake and retry."
    exit 2
fi

if [ -n "${VERSION_OVERRIDE}" ]; then
    VERSION="${VERSION_OVERRIDE}"
    log_line INFO "Using version override: ${VERSION}"
else
    VERSION=$(sed -n 's/^#define[[:space:]][[:space:]]*ZLIBNG_VERSION[[:space:]][[:space:]]*"\([^"]*\)".*/\1/p' "${ROOT_DIR}/zlib-ng.h.in" | head -n 1)
    if [ -z "${VERSION}" ]; then
        HEADER_ZLIB=$(sed -n 's/^#define[[:space:]][[:space:]]*ZLIB_VERSION[[:space:]][[:space:]]*"\([^"]*\)".*/\1/p' "${ROOT_DIR}/zlib.h.in" | head -n 1)
        if [ -n "${HEADER_ZLIB}" ]; then
            VERSION="${HEADER_ZLIB}.zlib-ng"
            log_line FALLBACK "Could not read ZLIBNG_VERSION. Fallback to ${VERSION} from zlib.h.in."
        else
            VERSION=$(date +%Y%m%d-%H%M%S)
            log_line FALLBACK "Could not read repo version. Fallback to datestamp ${VERSION}."
        fi
    else
        log_line INFO "Using repo version: ${VERSION}"
    fi
fi

if [ "${CLEAN}" -eq 1 ]; then
    log_line INFO "Cleaning build root ${BUILD_ROOT}"
    rm -rf "${BUILD_ROOT}"
    mkdir -p "${BUILD_ROOT}"
fi

JOBS_DEFAULT=$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)
JOBS=${JOBS:-${JOBS_DEFAULT}}

COMMON_DEFS="-DCMAKE_BUILD_TYPE=Release -DZLIB_COMPAT=ON -DZLIB_ALIASES=ON -DWITH_GZFILEOP=ON -DWITH_OPTIM=ON -DWITH_CRC32_CHORBA=ON -DWITH_REDUCED_MEM=OFF -DBUILD_TESTING=OFF -DWITH_GTEST=OFF -DWITH_BENCHMARKS=OFF"
X64_ISA_DEFS="-DWITH_AVX2=ON -DWITH_AVX512=ON -DWITH_AVX512VNNI=ON -DWITH_VPCLMULQDQ=ON"
ARM_ISA_DEFS="-DWITH_ARMV8=ON -DWITH_NEON=ON"

copy_unique() {
    src="$1"
    dest_dir="$2"
    target_name="${3:-$(basename "${src}")}"
    mkdir -p "${dest_dir}"
    cp -f "${src}" "${dest_dir}/${target_name}"
}

collect_artifacts() {
    build_dir="$1"
    platform_name="$2"
    arch_name="$3"
    used_c_std="$4"
    all_defs="$5"

    out_base="${RELEASE_DIR}/${platform_name}/${arch_name}/${VERSION}"
    out_static="${out_base}/static"
    out_dynamic="${out_base}/dynamic"
    out_include="${out_base}/include"

    mkdir -p "${out_static}" "${out_dynamic}" "${out_include}"

    find "${build_dir}" -type f \( -name '*.a' -o -name '*.lib' -o -name '*.so' -o -name '*.so.*' -o -name '*.dylib' -o -name '*.dll' -o -name '*.dll.a' \) | while IFS= read -r f; do
        name=$(basename "${f}")
        case "${name}" in
            *.dylib)
                copy_unique "${f}" "${out_dynamic}" "libz.dylib"
                ;;
            *.so|*.so.*)
                copy_unique "${f}" "${out_dynamic}" "libz.so"
                ;;
            *.dll)
                copy_unique "${f}" "${out_dynamic}" "zlib.dll"
                ;;
            *.dll.a)
                copy_unique "${f}" "${out_dynamic}" "zlib.dll.a"
                ;;
            *.lib)
                case "${name}" in
                    *static*) copy_unique "${f}" "${out_static}" "libz.a" ;;
                    *) copy_unique "${f}" "${out_dynamic}" "zlib.lib" ;;
                esac
                ;;
            *.a)
                copy_unique "${f}" "${out_static}" "libz.a"
                ;;
        esac
    done

    for h in zlib.h zconf.h zlib_name_mangling.h zlib_name_mangling-ng.h zlib-ng.h; do
        if [ -f "${build_dir}/${h}" ]; then
            copy_unique "${build_dir}/${h}" "${out_include}"
        elif [ -f "${ROOT_DIR}/${h}" ]; then
            copy_unique "${ROOT_DIR}/${h}" "${out_include}"
        fi
    done

    {
        echo "timestamp=${TIMESTAMP}"
        echo "platform=${platform_name}"
        echo "arch=${arch_name}"
        echo "version=${VERSION}"
        echo "c_standard=${used_c_std}"
        echo "cmake=$(cmake --version | head -n 1)"
        echo "git_commit=$(git -C "${ROOT_DIR}" rev-parse --short HEAD 2>/dev/null || echo unknown)"
        echo "definitions=${all_defs} -DCMAKE_C_STANDARD=${used_c_std}"
        echo "log_file=${LOG_FILE}"
    } > "${out_base}/build-info.txt"

    log_line INFO "Artifacts saved to ${out_base}"
}

configure_and_build_with_fallback() {
    build_dir="$1"
    defs="$2"

    standards=""
    case "${C_STANDARD_MODE}" in
        auto) standards="23 11" ;;
        23) standards="23" ;;
        11) standards="11" ;;
    esac

    first_std=$(printf '%s\n' ${standards} | head -n 1)
    last_std=$(printf '%s\n' ${standards} | tail -n 1)

    for std in ${standards}; do
        can_retry=0
        if [ "${C_STANDARD_MODE}" = "auto" ] && [ "${std}" != "${last_std}" ]; then
            can_retry=1
        fi

        rm -rf "${build_dir}"
        mkdir -p "${build_dir}"

        log_line INFO "Configuring with C standard ${std}"
        tmp_log="${LOG_DIR}/.cfg-$$-$(date +%s).log"
        if ! cmake -S "${ROOT_DIR}" -B "${build_dir}" ${defs} -DCMAKE_C_STANDARD="${std}" > "${tmp_log}" 2>&1; then
            cat "${tmp_log}" | tee -a "${LOG_FILE}"
            rm -f "${tmp_log}"
            if [ "${can_retry}" -eq 1 ]; then
                log_line FALLBACK "C${std} configure failed. Retrying with next standard."
                continue
            fi
            log_line ERROR "Configure failed with C${std}."
            return 1
        fi
        cat "${tmp_log}" | tee -a "${LOG_FILE}"
        rm -f "${tmp_log}"
        USED_C_STANDARD="${std}"
        if [ "${std}" != "${first_std}" ]; then
            log_line FALLBACK "Configured with C${std} after earlier standard failed."
        fi

        if run_and_log cmake --build "${build_dir}" --config Release -j "${JOBS}"; then
            return 0
        fi

        if [ "${can_retry}" -eq 1 ]; then
            log_line FALLBACK "Build failed with C${std}. Retrying with next standard."
            continue
        fi

        log_line ERROR "Build failed with C${std}."
        return 1
    done

    log_line ERROR "All C standard configure/build attempts failed."
    return 1
}

build_one() {
    platform_name="$1"
    arch_name="$2"
    extra_defs="$3"

    build_dir="${BUILD_ROOT}/${platform_name}/${arch_name}"
    defs="${COMMON_DEFS} ${extra_defs}"

    if ! configure_and_build_with_fallback "${build_dir}" "${defs}"; then
        return 1
    fi

    collect_artifacts "${build_dir}" "${platform_name}" "${arch_name}" "${USED_C_STANDARD}" "${defs}"
    return 0
}

build_mac() {
    log_line INFO "Starting mac/arm64 build"
    build_one mac arm64 "-DCMAKE_OSX_ARCHITECTURES=arm64 ${ARM_ISA_DEFS}"
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
    build_one ios arm64 "-DCMAKE_TOOLCHAIN_FILE=${IOS_TOOLCHAIN_FILE} -DCMAKE_SYSTEM_NAME=iOS -DCMAKE_OSX_SYSROOT=${ios_sysroot} -DCMAKE_OSX_ARCHITECTURES=arm64 ${ARM_ISA_DEFS}"
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
    build_one android arm64 "-DCMAKE_TOOLCHAIN_FILE=${ndk_toolchain} -DANDROID_ABI=arm64-v8a -DANDROID_PLATFORM=${android_platform} ${ARM_ISA_DEFS}"
}

build_linux() {
    [ "${ARCH}" = "arm64" ] && { build_linux_arm64; return $?; }
    log_line INFO "Starting linux/x64 build"

    uname_s=$(uname -s)
    extra=""

    if [ -n "${LINUX_X64_TOOLCHAIN_FILE:-}" ]; then
        if [ ! -f "${LINUX_X64_TOOLCHAIN_FILE}" ]; then
            log_line ERROR "LINUX_X64_TOOLCHAIN_FILE does not exist: ${LINUX_X64_TOOLCHAIN_FILE}"
            return 1
        fi
        extra="-DCMAKE_TOOLCHAIN_FILE=${LINUX_X64_TOOLCHAIN_FILE}"
    elif [ -n "${LINUX_X64_CC:-}" ]; then
        extra="-DCMAKE_SYSTEM_NAME=Linux -DCMAKE_C_COMPILER=${LINUX_X64_CC}"
    elif [ "${uname_s}" != "Linux" ]; then
        log_line ERROR "linux/x64 build on non-Linux host needs LINUX_X64_TOOLCHAIN_FILE or LINUX_X64_CC."
        return 1
    fi

    build_one linux x64 "${extra} ${X64_ISA_DEFS}"
}

# linux/arm64: native on an aarch64 Linux host, otherwise cross through zlib-ng's own
# cmake/toolchain-aarch64.cmake (aarch64-linux-gnu-gcc, apt: gcc-aarch64-linux-gnu).
build_linux_arm64() {
    log_line INFO "Starting linux/arm64 build"
    extra=""
    if [ -n "${LINUX_ARM64_TOOLCHAIN_FILE:-}" ] || [ "$(uname -s)" != "Linux" ] || [ "${HOST_ARCH}" != "arm64" ]; then
        toolchain="${LINUX_ARM64_TOOLCHAIN_FILE:-${ROOT_DIR}/cmake/toolchain-aarch64.cmake}"
        if [ ! -f "${toolchain}" ]; then
            log_line ERROR "linux/arm64 toolchain file not found: ${toolchain}"
            return 1
        fi
        if [ -z "${LINUX_ARM64_TOOLCHAIN_FILE:-}" ] && ! command -v aarch64-linux-gnu-gcc >/dev/null 2>&1; then
            log_line ERROR "aarch64-linux-gnu-gcc not found. Install gcc-aarch64-linux-gnu (or set LINUX_ARM64_TOOLCHAIN_FILE)."
            return 1
        fi
        extra="-DCMAKE_TOOLCHAIN_FILE=${toolchain}"
    fi
    build_one linux arm64 "${extra} ${ARM_ISA_DEFS}"
}

# windows/arm64 goes through llvm-mingw on every host: MSYS2's MinGW64 gcc (what the x64
# build uses) has no aarch64 target, and 7-Zip links this library the MinGW way, so an
# MSVC build here would leave it nothing it could link.
build_windows_arm64() {
    log_line INFO "Starting windows/arm64 build"
    toolchain="${WINDOWS_ARM64_TOOLCHAIN_FILE:-${ROOT_DIR}/cmake/toolchain-llvm-mingw-aarch64.cmake}"
    if [ ! -f "${toolchain}" ]; then
        log_line ERROR "windows/arm64 toolchain file not found: ${toolchain}"
        return 1
    fi
    if [ -z "${WINDOWS_ARM64_TOOLCHAIN_FILE:-}" ] && ! command -v aarch64-w64-mingw32-clang >/dev/null 2>&1; then
        log_line ERROR "aarch64-w64-mingw32-clang not found. Put llvm-mingw's bin/ on PATH (https://github.com/mstorsjo/llvm-mingw), or set WINDOWS_ARM64_TOOLCHAIN_FILE."
        return 1
    fi
    build_one windows arm64 "-DCMAKE_TOOLCHAIN_FILE=${toolchain} ${ARM_ISA_DEFS}"
}

build_windows() {
    [ "${ARCH}" = "arm64" ] && { build_windows_arm64; return $?; }
    log_line INFO "Starting windows/x64 build"

    if [ -z "${WINDOWS_TOOLCHAIN_FILE:-}" ]; then
        case "$(uname -s)" in
            MINGW*|MSYS*|CYGWIN*)
                log_line INFO "Native Windows host detected; using host MinGW-w64 toolchain directly (no cross toolchain file)."
                build_one windows x64 "${X64_ISA_DEFS}"
                return $?
                ;;
        esac
    fi

    toolchain=${WINDOWS_TOOLCHAIN_FILE:-${ROOT_DIR}/cmake/toolchain-llvm-mingw-x86_64.cmake}

    if [ ! -f "${toolchain}" ]; then
        log_line ERROR "Windows toolchain file not found: ${toolchain}"
        return 1
    fi

    build_one windows x64 "-DCMAKE_TOOLCHAIN_FILE=${toolchain} ${X64_ISA_DEFS}"
}

platforms="${PLATFORM}"

failures=""

for p in ${platforms}; do
    case "${p}" in
        mac)
            if ! build_mac; then failures="${failures} mac/arm64"; fi
            ;;
        ios)
            if ! build_ios; then failures="${failures} ios/arm64"; fi
            ;;
        android)
            if ! build_android; then failures="${failures} android/arm64"; fi
            ;;
        linux)
            if ! build_linux; then failures="${failures} linux/${ARCH}"; fi
            ;;
        windows)
            if ! build_windows; then failures="${failures} windows/${ARCH}"; fi
            ;;
    esac
done

if [ -n "${failures}" ]; then
    log_line ERROR "Build completed with failures:${failures}"
    log_line ERROR "See full details in ${LOG_FILE}"
    exit 1
fi

log_line INFO "Build completed successfully for: ${platforms}/${ARCH}"
log_line INFO "Release root: ${RELEASE_DIR}"
log_line INFO "Log file: ${LOG_FILE}"
