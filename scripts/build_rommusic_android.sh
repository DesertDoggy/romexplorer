#!/usr/bin/env bash
# Builds the rommusic Android app (arm64 APK): the native libraries for android/arm64 if asked,
# then the Dioxus app with `dx`, then the APK again with those libraries inside it. With --cli,
# also the command-line tool for Android (rommusic) with the libraries in lib/ beside it. The
# same steps and options as scripts/build_chdlite_android.sh, for rommusic-cli.
#
# Usage: scripts/build_rommusic_android.sh [--natives] [--release] [--cli|--cli-only] [--check]
#   --natives   first build every native library rommusic uses (the players: ffmpeg,
#               vgmstream, rommusic; the disc readers: zlib-ng, chdman-simd, dolphin, cemu)
#               for android/arm64 (scripts/build_native_deps.sh --only ...) and deploy them to
#               app_root_ex/lib/android/arm64/. Needed once, and after a native change.
#   --release   a release APK (`dx bundle`), signed when the environment names a keystore
#               (below), otherwise unsigned (app-release-unsigned.apk, which Android will
#               not install).
#   --cli       also the command-line tool for android/arm64 (cargo, the NDK's clang as
#               linker), into release/rommusic/<rommusic version>/android/arm64/, with the
#               libraries it plays with in lib/android/arm64/ beside it -- copy the folder as
#               a whole and run it from a terminal app on the device (Termux) or `adb shell`.
#               Release builds with --release, debug otherwise.
#   --cli-only  the command-line tool alone, no APK.
#   --check     report what is found and missing, build nothing.
#
# What it needs (none of it is installed by this script): the same as
# scripts/build_chdlite_android.sh -- the Android SDK (ANDROID_HOME), NDK r26+
# (ANDROID_NDK_HOME), a JDK 17+, `rustup target add aarch64-linux-android` and the Dioxus CLI
# 0.7 -- and for --natives what scripts/build_native_deps.sh needs (on Windows: an MSYS2
# MINGW64 shell with cmake, ninja and make; FFmpeg's configure runs there too).
#
# The libraries are built for API level 28 (Android 9), the app's minimum: libvgm's text
# conversion uses iconv, which Android's libc has only from 28 on.
#
# To sign a release APK: ROMMUSIC_KEYSTORE, ROMMUSIC_KEY_ALIAS and ROMMUSIC_KEYSTORE_PASS (plus
# ROMMUSIC_KEY_PASS when the key has a password of its own). Unset, the CHDLITE_ ones are used,
# so one keystore can sign both apps. The passwords go to apksigner through the environment
# (`env:`), never on a command line. Every update must be signed with the same key.
#
# The APK ends up under target/dx/rommusic-gui/<debug|release>/android/app/app/build/outputs/.
# The command-line tool (--cli) under release/rommusic/<rommusic version>/android/arm64/.
# Sideload it (`adb install -r <apk>`). On first start rommusic asks for All files access.
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CRATE_DIR="$ROOT_DIR/rommusic-cli"
TARGET=aarch64-linux-android
ABI=arm64-v8a

NATIVES=0 RELEASE=0 CHECK=0 CLI=0 APP=1
# The API level the command-line tool is linked for: 28 (Android 9), as the libraries are.
CLI_API=28
while [[ $# -gt 0 ]]; do
    case "$1" in
        --natives) NATIVES=1 ;;
        --release) RELEASE=1 ;;
        --cli) CLI=1 ;;
        --cli-only) CLI=1 APP=0 ;;
        --check) CHECK=1 ;;
        -h|--help) sed -n '2,39p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
# What the APK carries: the two players (required), then the disc readers and 7-Zip (zip and
# 7z sets), each needed only for some inputs. app_root_ex/lib/android/arm64/ is shared with
# chdlite, so only these go in. Names as the APK carries them: Android extracts only lib*.so,
# so 7-Zip's 7z.so goes in as lib7z.so (src/natives.rs links it back under 7z.so).
APK_LIBS=(librommusic.so librvgmstream.so libchdman.so libdolphinrvz.so libcemuwua.so libz.so lib7z.so)
# The file in $LIB_DIR an APK name comes from.
lib_source() { if [[ "$1" == lib7z.so ]]; then echo 7z.so; else echo "$1"; fi; }
if [[ "$NATIVES" -eq 1 ]]; then
    ANDROID_API="${ANDROID_API:-28}" bash "$SCRIPT_DIR/build_native_deps.sh" --only zlib-ng,7-Zip-zstd,chdman-simd,dolphin,ffmpeg,vgmstream,rommusic,cemu android arm64
fi
for lib in librommusic.so librvgmstream.so; do
    [[ -f "$LIB_DIR/$lib" ]] || echo "warning: $LIB_DIR/$lib is not built (run with --natives); rommusic plays nothing without it" >&2
done

# --- the command-line tool ---------------------------------------------------------------------
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
    CLI_PROFILE=debug CARGO_PROFILE=()
    if [[ "$RELEASE" -eq 1 ]]; then CLI_PROFILE=release CARGO_PROFILE=(--release); fi
    (cd "$ROOT_DIR" && cargo build -p rommusic-cli --target "$TARGET" "${CARGO_PROFILE[@]}" --bin rommusic)
    CLI_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$CRATE_DIR/Cargo.toml" | head -1)"
    CLI_OUT="$ROOT_DIR/release/rommusic/$CLI_VERSION/android/arm64"
    # rommusic looks for its libraries in lib/<os>/<arch>/ beside itself, as on a desktop.
    CLI_LIB="$CLI_OUT/lib/android/arm64"
    rm -rf "$CLI_LIB"
    mkdir -p "$CLI_LIB"
    cp -f "$ROOT_DIR/target/$TARGET/$CLI_PROFILE/rommusic" "$CLI_OUT/rommusic"
    # Under the names romexplorer loads (7z.so), as on a desktop.
    for name in "${APK_LIBS[@]}"; do
        source_name="$(lib_source "$name")"
        [[ -f "$LIB_DIR/$source_name" ]] && cp -f "$LIB_DIR/$source_name" "$CLI_LIB/"
    done
    echo
    echo "command-line tool ($CLI_PROFILE):"
    (cd "$CLI_OUT" && find . -type f | sort | sed 's/^\./  /')
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
(cd "$CRATE_DIR" && dx "${DX_CMD[@]}" --android --target "$TARGET" --bin rommusic-gui --features gui)

# --- the libraries and the launcher icon into the APK -------------------------------------------
# dx packages only the Rust library, and on Android it ignores Dioxus.toml's `icon`: it always
# writes its own launcher icon (the Android robot). The natives go beside the Rust library in
# the Gradle project's jniLibs, rommusic-cli/android/res/ (scripts/make_android_icons.py) goes over
# dx's icon, and Gradle runs again (incrementally) to put both in the APK.
PROJECT="$ROOT_DIR/target/dx/rommusic-gui/$PROFILE/android/app"
JNI_DIR="$PROJECT/app/src/main/jniLibs/$ABI"
[[ -d "$PROJECT" ]] || { echo "error: dx left no Gradle project at $PROJECT" >&2; exit 1; }
mkdir -p "$JNI_DIR"
copied=0
for name in "${APK_LIBS[@]}"; do
    lib="$LIB_DIR/$(lib_source "$name")"
    [[ -f "$lib" ]] || continue
    cp -f "$lib" "$JNI_DIR/$name"
    copied=$((copied + 1))
done
echo "copied $copied native librar$([[ $copied -eq 1 ]] && echo y || echo ies) into $JNI_DIR"

ICON_DIR="$CRATE_DIR/android/res"
RES_DIR="$PROJECT/app/src/main/res"
[[ -f "$ICON_DIR/mipmap-anydpi-v26/ic_launcher.xml" ]] || { echo "error: no launcher icon in $ICON_DIR (scripts/make_android_icons.py)" >&2; exit 1; }
# dx's icons are .webp, ours .png under the same names; Gradle refuses two resources of one name.
rm -f "$RES_DIR"/mipmap-*/ic_launcher.webp
cp -Rf "$ICON_DIR/." "$RES_DIR/"
echo "launcher icon from $ICON_DIR"

# The music maps (app_root_ex/assets/music_maps*.zst, numbers only): where each matched game's
# music is in its ROM, so a cartridge plays from its map rather than a search from boot. Into
# the APK's assets; at each start src/natives.rs copies them to where rom_songs reads them.
ASSETS_DIR="$PROJECT/app/src/main/assets"
mkdir -p "$ASSETS_DIR"
rm -f "$ASSETS_DIR"/music_maps*.zst
maps=0
for map in "$ROOT_DIR"/app_root_ex/assets/music_maps*.zst; do
    [[ -f "$map" ]] || continue
    cp -f "$map" "$ASSETS_DIR/"
    maps=$((maps + 1))
done
if [[ "$maps" -gt 0 ]]; then
    echo "copied $maps music map bundle(s) into $ASSETS_DIR"
else
    echo "warning: no app_root_ex/assets/music_maps*.zst: cartridges fall back to the search from boot" >&2
fi

TASK=assembleDebug
[[ "$RELEASE" -eq 1 ]] && TASK=assembleRelease
if [[ "$WINDOWS" -eq 1 ]]; then
    # Run by path: `cmd //c gradlew.bat` from Git Bash did not find it in the current folder.
    (cd "$PROJECT" && ./gradlew.bat "$TASK")
else
    (cd "$PROJECT" && sh ./gradlew "$TASK")
fi

# --- signing --------------------------------------------------------------------------------------
# Gradle leaves the release APK unsigned (dx sets up no signing config). zipalign, then
# apksigner with the keystore the environment names, into app-release.apk; then verified.
if [[ "$RELEASE" -eq 1 ]]; then
    OUT="$PROJECT/app/build/outputs/apk/release"
    # A signed APK from an earlier run must not outlive a build that was not signed.
    rm -f "$OUT/app-release.apk" "$OUT/app-release-aligned.apk"
    # rommusic's own keystore, else chdlite's.
    if [[ -z "${ROMMUSIC_KEYSTORE:-}" ]]; then
        ROMMUSIC_KEYSTORE="${CHDLITE_KEYSTORE:-}" ROMMUSIC_KEY_ALIAS="${CHDLITE_KEY_ALIAS:-}"
        ROMMUSIC_KEYSTORE_PASS="${CHDLITE_KEYSTORE_PASS:-}" ROMMUSIC_KEY_PASS="${CHDLITE_KEY_PASS:-}"
    fi
    export ROMMUSIC_KEYSTORE_PASS ROMMUSIC_KEY_PASS
    if [[ -n "${ROMMUSIC_KEYSTORE:-}" && -n "${ROMMUSIC_KEY_ALIAS:-}" && -n "${ROMMUSIC_KEYSTORE_PASS:-}" ]]; then
        [[ -f "$ROMMUSIC_KEYSTORE" ]] || { echo "error: the keystore names no file: $ROMMUSIC_KEYSTORE" >&2; exit 1; }
        BT="$(ls -1d "$ANDROID_HOME"/build-tools/*/ 2>/dev/null | sort -V | tail -1)"
        BT="${BT%/}"
        ZIPALIGN="$BT/zipalign"; [[ -f "$ZIPALIGN.exe" ]] && ZIPALIGN="$ZIPALIGN.exe"
        APKSIGNER="$BT/apksigner"; [[ -f "$APKSIGNER.bat" ]] && APKSIGNER="$APKSIGNER.bat"
        [[ -f "$ZIPALIGN" && -f "$APKSIGNER" ]] || { echo "error: no zipalign / apksigner in $BT" >&2; exit 1; }
        "$ZIPALIGN" -f -p 4 "$OUT/app-release-unsigned.apk" "$OUT/app-release-aligned.apk"
        KEY_PASS=()
        [[ -n "${ROMMUSIC_KEY_PASS:-}" ]] && KEY_PASS=(--key-pass env:ROMMUSIC_KEY_PASS)
        "$APKSIGNER" sign --ks "$ROMMUSIC_KEYSTORE" --ks-key-alias "$ROMMUSIC_KEY_ALIAS" --ks-pass env:ROMMUSIC_KEYSTORE_PASS "${KEY_PASS[@]}" \
            --out "$OUT/app-release.apk" "$OUT/app-release-aligned.apk"
        rm -f "$OUT/app-release-aligned.apk" "$OUT/app-release.apk.idsig"
        "$APKSIGNER" verify --print-certs "$OUT/app-release.apk" | grep -E 'Signer.*certificate (DN|SHA-256)' || { echo "error: apksigner could not verify $OUT/app-release.apk" >&2; exit 1; }
        echo "signed: $OUT/app-release.apk"
    else
        echo "note: the release APK is unsigned (Android will not install it). To sign it, set" >&2
        echo "      ROMMUSIC_KEYSTORE, ROMMUSIC_KEY_ALIAS and ROMMUSIC_KEYSTORE_PASS (or the CHDLITE_ ones; see --help)." >&2
    fi
fi

echo
echo "APK(s):"
find "$PROJECT/app/build/outputs" -name '*.apk' 2>/dev/null | sed 's/^/  /' || true
