#!/usr/bin/env bash
# Builds the chdlite Android app (arm64 APK): the native libraries for android/arm64 if asked,
# then the Dioxus app with `dx`, then the APK again with those libraries inside it.
#
# Usage: scripts/build_chdlite_android.sh [--natives] [--release] [--check]
#   --natives   first build zlib-ng, chdman-simd and dolphin for android/arm64
#               (scripts/build_native_deps.sh --only ...) and deploy them to
#               app_root_ex/lib/android/arm64/. Needed once, and after a native change.
#   --release   a release APK (`dx bundle`); signing comes from Dioxus.toml's
#               [android.signing] if set there, otherwise the APK is unsigned.
#   --check     report what is found and missing, build nothing.
#
# What it needs (none of it is installed by this script):
#   * Android SDK with build-tools and platforms;android-35: ANDROID_HOME (or
#     ANDROID_SDK_ROOT), else the usual per-user location.
#   * Android NDK r26+: ANDROID_NDK_HOME, else the newest under $ANDROID_HOME/ndk/.
#   * A JDK 17+: JAVA_HOME, else `java` on PATH.
#   * `rustup target add aarch64-linux-android`, and the Dioxus CLI (`cargo install
#     dioxus-cli`, the same 0.7 version as the dioxus crate).
#   * For --natives, everything scripts/build_native_deps.sh needs (on Windows: an MSYS2
#     MINGW64 shell with cmake and ninja; see deps/README.md).
#   * Network access on the first run: Gradle fetches itself and the Android build plugins.
#
# The APK ends up under target/dx/chdlite-gui/<debug|release>/android/app/app/build/outputs/.
# Sideload it (`adb install -r <apk>`). On first start chdlite asks for All files access.
#
# Not built for Android yet: cemuwua, exiso and psxpbp (their build scripts have no android
# arm). Without them the app says so for Wii U, Xbox and PBP inputs and converts the rest.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CRATE_DIR="$ROOT_DIR/chdlite"
TARGET=aarch64-linux-android
ABI=arm64-v8a

NATIVES=0 RELEASE=0 CHECK=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --natives) NATIVES=1 ;;
        --release) RELEASE=1 ;;
        --check) CHECK=1 ;;
        -h|--help) sed -n '2,30p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "error: unknown option '$1' (try --help)" >&2; exit 2 ;;
    esac
    shift
done

case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) WINDOWS=1 ;;
    *) WINDOWS=0 ;;
esac

# --- the toolchain ----------------------------------------------------------------------------
MISSING=()

if [[ -z "${ANDROID_HOME:-}" ]]; then
    for candidate in "${ANDROID_SDK_ROOT:-}" "${LOCALAPPDATA:-}/Android/Sdk" "$HOME/Android/Sdk" "$HOME/Library/Android/sdk"; do
        if [[ -n "$candidate" && -d "$candidate/platforms" ]]; then ANDROID_HOME="$candidate"; break; fi
    done
fi
if [[ -n "${ANDROID_HOME:-}" && -d "$ANDROID_HOME" ]]; then
    export ANDROID_HOME ANDROID_SDK_ROOT="$ANDROID_HOME"
    [[ -d "$ANDROID_HOME/build-tools" ]] || MISSING+=("Android SDK build-tools (sdkmanager \"build-tools;35.0.0\")")
    [[ -d "$ANDROID_HOME/platforms/android-35" ]] || echo "note: no platforms/android-35 in the SDK; Gradle will try to install it (sdkmanager \"platforms;android-35\")" >&2
else
    MISSING+=("Android SDK (set ANDROID_HOME)")
fi

if [[ -z "${ANDROID_NDK_HOME:-}" && -n "${ANDROID_HOME:-}" && -d "$ANDROID_HOME/ndk" ]]; then
    newest="$(ls -1d "$ANDROID_HOME"/ndk/*/ 2>/dev/null | sort -V | tail -1 || true)"
    [[ -n "$newest" ]] && ANDROID_NDK_HOME="${newest%/}"
fi
if [[ -n "${ANDROID_NDK_HOME:-}" && -d "$ANDROID_NDK_HOME/toolchains/llvm" ]]; then
    export ANDROID_NDK_HOME NDK_HOME="$ANDROID_NDK_HOME" ANDROID_NDK_ROOT="$ANDROID_NDK_HOME"
else
    MISSING+=("Android NDK r26+ (set ANDROID_NDK_HOME, or sdkmanager \"ndk;<version>\")")
fi

if [[ -n "${JAVA_HOME:-}" && -x "$JAVA_HOME/bin/java" || -n "${JAVA_HOME:-}" && -f "$JAVA_HOME/bin/java.exe" ]]; then
    export JAVA_HOME
elif ! command -v java >/dev/null 2>&1; then
    MISSING+=("a JDK 17+ (set JAVA_HOME)")
fi

if ! rustup target list --installed 2>/dev/null | grep -qx "$TARGET"; then
    MISSING+=("the Rust target (rustup target add $TARGET)")
fi
command -v dx >/dev/null 2>&1 || MISSING+=("the Dioxus CLI (cargo install dioxus-cli --version 0.7.10)")

echo "SDK:  ${ANDROID_HOME:-<none>}"
echo "NDK:  ${ANDROID_NDK_HOME:-<none>}"
echo "JDK:  ${JAVA_HOME:-$(command -v java || echo '<none>')}"
echo "dx:   $(dx --version 2>/dev/null || echo '<none>')"
if [[ ${#MISSING[@]} -gt 0 ]]; then
    echo >&2
    echo "missing:" >&2
    printf '  - %s\n' "${MISSING[@]}" >&2
    exit 1
fi
[[ "$CHECK" -eq 1 ]] && { echo "everything needed is there"; exit 0; }

# --- native libraries ---------------------------------------------------------------------------
LIB_DIR="$ROOT_DIR/app_root_ex/lib/android/arm64"
if [[ "$NATIVES" -eq 1 ]]; then
    bash "$SCRIPT_DIR/build_native_deps.sh" --only zlib-ng,chdman-simd,dolphin android arm64
fi
for lib in libchdman.so libdolphinrvz.so; do
    [[ -f "$LIB_DIR/$lib" ]] || echo "warning: $LIB_DIR/$lib is not built (run with --natives); the APK can't convert without it" >&2
done

# --- the app ------------------------------------------------------------------------------------
# dx canonicalizes paths in its session cache under TEMP, which fails ("Incorrect function")
# on a volume that does not support it, such as some RAM disks. Keep it in target/.
if [[ "$WINDOWS" -eq 1 ]]; then
    TMP_DIR="$ROOT_DIR/target/tmp-dx"
    mkdir -p "$TMP_DIR"
    export TEMP="$TMP_DIR" TMP="$TMP_DIR"
fi

PROFILE=debug
DX_CMD=(build)
if [[ "$RELEASE" -eq 1 ]]; then
    PROFILE=release
    DX_CMD=(bundle --release)
fi
(cd "$CRATE_DIR" && dx "${DX_CMD[@]}" --android --target "$TARGET" --bin chdlite-gui --features gui)

# --- the libraries into the APK -----------------------------------------------------------------
# dx packages only the Rust library. The natives go beside it in the Gradle project's jniLibs,
# and Gradle runs again (incrementally) to put them in the APK.
PROJECT="$ROOT_DIR/target/dx/chdlite-gui/$PROFILE/android/app"
JNI_DIR="$PROJECT/app/src/main/jniLibs/$ABI"
[[ -d "$PROJECT" ]] || { echo "error: dx left no Gradle project at $PROJECT" >&2; exit 1; }
mkdir -p "$JNI_DIR"
copied=0
for lib in "$LIB_DIR"/*.so; do
    [[ -f "$lib" ]] || continue
    cp -f "$lib" "$JNI_DIR/"
    copied=$((copied + 1))
done
echo "copied $copied native librar$([[ $copied -eq 1 ]] && echo y || echo ies) into $JNI_DIR"

if [[ "$copied" -gt 0 ]]; then
    TASK=assembleDebug
    [[ "$RELEASE" -eq 1 ]] && TASK=assembleRelease
    if [[ "$WINDOWS" -eq 1 ]]; then
        (cd "$PROJECT" && cmd //c gradlew.bat "$TASK")
    else
        (cd "$PROJECT" && sh ./gradlew "$TASK")
    fi
fi

echo
echo "APK(s):"
find "$PROJECT/app/build/outputs" -name '*.apk' 2>/dev/null | sed 's/^/  /' || true
