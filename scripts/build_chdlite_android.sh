#!/usr/bin/env bash
# Builds the chdlite Android app (arm64 APK): the native libraries for android/arm64 if asked,
# then the Dioxus app with `dx`, then the APK again with those libraries inside it. With --cli,
# also the command-line tool for Android (chdlite), one file with the libraries inside it.
# Only chdlite: chdcomp / chdread / chdhash are the same program under another name, which
# matters for drag and drop on a desktop, not in a phone's terminal, and each would carry its
# own ~44 MB copy of the libraries.
#
# Usage: scripts/build_chdlite_android.sh [--natives] [--release] [--cli|--cli-only] [--check]
#   --natives   first build every native library chdlite uses (zlib-ng, chdman-simd, dolphin,
#               cemu, extract-xiso, beetle-psx-libretro) for android/arm64
#               (scripts/build_native_deps.sh --only ...) and deploy them to
#               app_root_ex/lib/android/arm64/. Needed once, and after a native change.
#   --release   a release APK (`dx bundle`), signed when the environment names a keystore
#               (below), otherwise unsigned (app-release-unsigned.apk, which Android will
#               not install).
#   --cli       also the command-line tool for android/arm64 (cargo, the NDK's clang as
#               linker, `bundled-natives`), into release/chdlite/<chdlite version>/android/arm64/
#               -- run them from a terminal app on the device (Termux) or `adb shell`.
#               Release builds with --release, debug otherwise.
#   --cli-only  the command-line tool alone, no APK.
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
#   * To sign a release APK: CHDLITE_KEYSTORE (the .jks, kept outside the repo),
#     CHDLITE_KEY_ALIAS and CHDLITE_KEYSTORE_PASS, plus CHDLITE_KEY_PASS when the key has a
#     password of its own. The passwords go to apksigner through the environment
#     (`env:`), never on a command line. Every update must be signed with the same key.
#
# The APK ends up under target/dx/chdlite-gui/<debug|release>/android/app/app/build/outputs/.
# The command-line tool (--cli) under release/chdlite/<chdlite version>/android/arm64/.
# Sideload it (`adb install -r <apk>`). On first start chdlite asks for All files access.
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CRATE_DIR="$ROOT_DIR/chdlite"
TARGET=aarch64-linux-android
ABI=arm64-v8a

NATIVES=0 RELEASE=0 CHECK=0 CLI=0 APP=1
# The API level the command-line tools are linked for (Android 10); the app's own comes from dx.
CLI_API=29
while [[ $# -gt 0 ]]; do
    case "$1" in
        --natives) NATIVES=1 ;;
        --release) RELEASE=1 ;;
        --cli) CLI=1 ;;
        --cli-only) CLI=1 APP=0 ;;
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
[[ "$APP" -eq 1 ]] && { command -v dx >/dev/null 2>&1 || MISSING+=("the Dioxus CLI (cargo install dioxus-cli --version 0.7.10)"); }

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
    bash "$SCRIPT_DIR/build_native_deps.sh" --only zlib-ng,chdman-simd,dolphin,cemu,extract-xiso,beetle-psx-libretro android arm64
fi
for lib in libchdman.so libdolphinrvz.so; do
    [[ -f "$LIB_DIR/$lib" ]] || echo "warning: $LIB_DIR/$lib is not built (run with --natives); the APK can't convert without it" >&2
done

# --- the command-line tools --------------------------------------------------------------------
if [[ "$CLI" -eq 1 ]]; then
    case "$(uname -s)" in
        MINGW*|MSYS*|CYGWIN*) HOST_TAG=windows-x86_64; EXE=.cmd ;;
        Darwin) HOST_TAG=darwin-x86_64; EXE= ;;
        *) HOST_TAG=linux-x86_64; EXE= ;;
    esac
    BIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/bin"
    LINKER="$BIN/aarch64-linux-android${CLI_API}-clang$EXE"
    [[ -f "$LINKER" ]] || { echo "error: no NDK linker at $LINKER" >&2; exit 1; }
    export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$LINKER"
    export CC_aarch64_linux_android="$LINKER" CXX_aarch64_linux_android="$BIN/aarch64-linux-android${CLI_API}-clang++$EXE"
    export AR_aarch64_linux_android="$BIN/llvm-ar${EXE:+.exe}"
    export CHDLITE_NATIVE_LIB_DIR="$LIB_DIR"
    CLI_PROFILE=debug CARGO_PROFILE=()
    if [[ "$RELEASE" -eq 1 ]]; then CLI_PROFILE=release CARGO_PROFILE=(--release); fi
    (cd "$ROOT_DIR" && cargo build -p chdlite --target "$TARGET" "${CARGO_PROFILE[@]}" --features bundled-natives --bin chdlite)
    CLI_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$CRATE_DIR/Cargo.toml" | head -1)"
    CLI_OUT="$ROOT_DIR/release/chdlite/$CLI_VERSION/android/arm64"
    mkdir -p "$CLI_OUT"
    rm -f "$CLI_OUT"/chdcomp "$CLI_OUT"/chdread "$CLI_OUT"/chdhash
    cp -f "$ROOT_DIR/target/$TARGET/$CLI_PROFILE/chdlite" "$CLI_OUT/chdlite"
    echo
    echo "command-line tool ($CLI_PROFILE):"
    ls -l "$CLI_OUT" | sed 's/^/  /'
fi
[[ "$APP" -eq 1 ]] || exit 0

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
        # Run by path: `cmd //c gradlew.bat` from Git Bash did not find it in the current folder.
        (cd "$PROJECT" && ./gradlew.bat "$TASK")
    else
        (cd "$PROJECT" && sh ./gradlew "$TASK")
    fi
fi

# --- signing --------------------------------------------------------------------------------------
# Gradle leaves the release APK unsigned (dx sets up no signing config). zipalign, then
# apksigner with the keystore the environment names, into app-release.apk; then verified.
if [[ "$RELEASE" -eq 1 ]]; then
    OUT="$PROJECT/app/build/outputs/apk/release"
    # A signed APK from an earlier run must not outlive a build that was not signed.
    rm -f "$OUT/app-release.apk" "$OUT/app-release-aligned.apk"
    if [[ -n "${CHDLITE_KEYSTORE:-}" && -n "${CHDLITE_KEY_ALIAS:-}" && -n "${CHDLITE_KEYSTORE_PASS:-}" ]]; then
        [[ -f "$CHDLITE_KEYSTORE" ]] || { echo "error: CHDLITE_KEYSTORE names no file: $CHDLITE_KEYSTORE" >&2; exit 1; }
        BT="$(ls -1d "$ANDROID_HOME"/build-tools/*/ 2>/dev/null | sort -V | tail -1)"
        BT="${BT%/}"
        ZIPALIGN="$BT/zipalign"; [[ -f "$ZIPALIGN.exe" ]] && ZIPALIGN="$ZIPALIGN.exe"
        APKSIGNER="$BT/apksigner"; [[ -f "$APKSIGNER.bat" ]] && APKSIGNER="$APKSIGNER.bat"
        [[ -f "$ZIPALIGN" && -f "$APKSIGNER" ]] || { echo "error: no zipalign / apksigner in $BT" >&2; exit 1; }
        "$ZIPALIGN" -f -p 4 "$OUT/app-release-unsigned.apk" "$OUT/app-release-aligned.apk"
        KEY_PASS=()
        [[ -n "${CHDLITE_KEY_PASS:-}" ]] && KEY_PASS=(--key-pass env:CHDLITE_KEY_PASS)
        "$APKSIGNER" sign --ks "$CHDLITE_KEYSTORE" --ks-key-alias "$CHDLITE_KEY_ALIAS" --ks-pass env:CHDLITE_KEYSTORE_PASS "${KEY_PASS[@]}" \
            --out "$OUT/app-release.apk" "$OUT/app-release-aligned.apk"
        rm -f "$OUT/app-release-aligned.apk" "$OUT/app-release.apk.idsig"
        "$APKSIGNER" verify --print-certs "$OUT/app-release.apk" | grep -E 'Signer.*certificate (DN|SHA-256)' || { echo "error: apksigner could not verify $OUT/app-release.apk" >&2; exit 1; }
        echo "signed: $OUT/app-release.apk"
    else
        echo "note: the release APK is unsigned (Android will not install it). To sign it, set" >&2
        echo "      CHDLITE_KEYSTORE, CHDLITE_KEY_ALIAS and CHDLITE_KEYSTORE_PASS (see --help)." >&2
    fi
fi

echo
echo "APK(s):"
find "$PROJECT/app/build/outputs" -name '*.apk' 2>/dev/null | sed 's/^/  /' || true
