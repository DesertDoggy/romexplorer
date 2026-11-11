#!/usr/bin/env bash
# Builds the romcompanion Android app (arm64 APK) with `dx`: romexplorer on a phone, the app's
# Big Picture screens over a synced copy of a server's or client's catalogue. The same steps
# as scripts/build_chdlite_android.sh, minus the native libraries: romcompanion has none (no
# scans, converts or ROM music on the phone; SQLite is compiled into the Rust library).
#
# Usage: scripts/build_romcompanion_android.sh [--release] [--check]
#   --release   a release APK (`dx bundle`), signed when the environment names a keystore
#               (below), otherwise unsigned (app-release-unsigned.apk, which Android will
#               not install).
#   --check     report what is found and missing, build nothing.
#
# What it needs (none of it is installed by this script): the same as
# scripts/build_chdlite_android.sh -- the Android SDK (ANDROID_HOME), NDK r26+
# (ANDROID_NDK_HOME), a JDK 17+, `rustup target add aarch64-linux-android` and the Dioxus CLI
# 0.7 -- and network access on the first run (Gradle fetches itself and its plugins).
#
# To sign a release APK: ROMCOMPANION_KEYSTORE, ROMCOMPANION_KEY_ALIAS and
# ROMCOMPANION_KEYSTORE_PASS (plus ROMCOMPANION_KEY_PASS when the key has a password of its
# own). Unset, the CHDLITE_ ones are used, so one keystore can sign every app. The passwords
# go to apksigner through the environment (`env:`), never on a command line. Every update
# must be signed with the same key.
#
# The APK ends up under target/dx/romcompanion/<debug|release>/android/app/app/build/outputs/.
# Sideload it (`adb install -r <apk>`). On first start it asks for a server to connect to.
#
# iOS: the same crate builds with `dx bundle --ios` on a Mac; not shipped yet.
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
CRATE_DIR="$ROOT_DIR/romcompanion"
TARGET=aarch64-linux-android

RELEASE=0 CHECK=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --release) RELEASE=1 ;;
        --check) CHECK=1 ;;
        -h|--help) sed -n '2,28p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
(cd "$CRATE_DIR" && dx "${DX_CMD[@]}" --android --target "$TARGET" --bin romcompanion)

PROJECT="$ROOT_DIR/target/dx/romcompanion/$PROFILE/android/app"
[[ -d "$PROJECT" ]] || { echo "error: dx left no Gradle project at $PROJECT" >&2; exit 1; }

# --- signing --------------------------------------------------------------------------------------
# Gradle leaves the release APK unsigned (dx sets up no signing config). zipalign, then
# apksigner with the keystore the environment names, into app-release.apk; then verified.
if [[ "$RELEASE" -eq 1 ]]; then
    KEYSTORE="${ROMCOMPANION_KEYSTORE:-${CHDLITE_KEYSTORE:-}}"
    KEY_ALIAS="${ROMCOMPANION_KEY_ALIAS:-${CHDLITE_KEY_ALIAS:-}}"
    export ROMCOMPANION_KEYSTORE_PASS="${ROMCOMPANION_KEYSTORE_PASS:-${CHDLITE_KEYSTORE_PASS:-}}"
    export ROMCOMPANION_KEY_PASS="${ROMCOMPANION_KEY_PASS:-${CHDLITE_KEY_PASS:-}}"
    OUT="$PROJECT/app/build/outputs/apk/release"
    # A signed APK from an earlier run must not outlive a build that was not signed.
    rm -f "$OUT/app-release.apk" "$OUT/app-release-aligned.apk"
    if [[ -n "$KEYSTORE" && -n "$KEY_ALIAS" && -n "$ROMCOMPANION_KEYSTORE_PASS" ]]; then
        [[ -f "$KEYSTORE" ]] || { echo "error: the keystore names no file: $KEYSTORE" >&2; exit 1; }
        BT="$(ls -1d "$ANDROID_HOME"/build-tools/*/ 2>/dev/null | sort -V | tail -1)"
        BT="${BT%/}"
        ZIPALIGN="$BT/zipalign"; [[ -f "$ZIPALIGN.exe" ]] && ZIPALIGN="$ZIPALIGN.exe"
        APKSIGNER="$BT/apksigner"; [[ -f "$APKSIGNER.bat" ]] && APKSIGNER="$APKSIGNER.bat"
        [[ -f "$ZIPALIGN" && -f "$APKSIGNER" ]] || { echo "error: no zipalign / apksigner in $BT" >&2; exit 1; }
        "$ZIPALIGN" -f -p 4 "$OUT/app-release-unsigned.apk" "$OUT/app-release-aligned.apk"
        KEY_PASS=()
        [[ -n "$ROMCOMPANION_KEY_PASS" ]] && KEY_PASS=(--key-pass env:ROMCOMPANION_KEY_PASS)
        "$APKSIGNER" sign --ks "$KEYSTORE" --ks-key-alias "$KEY_ALIAS" --ks-pass env:ROMCOMPANION_KEYSTORE_PASS "${KEY_PASS[@]}" \
            --out "$OUT/app-release.apk" "$OUT/app-release-aligned.apk"
        rm -f "$OUT/app-release-aligned.apk" "$OUT/app-release.apk.idsig"
        "$APKSIGNER" verify --print-certs "$OUT/app-release.apk" | grep -E 'Signer.*certificate (DN|SHA-256)' || { echo "error: apksigner could not verify $OUT/app-release.apk" >&2; exit 1; }
        echo "signed: $OUT/app-release.apk"
    else
        echo "note: the release APK is unsigned (Android will not install it). To sign it, set" >&2
        echo "      ROMCOMPANION_KEYSTORE, ROMCOMPANION_KEY_ALIAS and ROMCOMPANION_KEYSTORE_PASS (see --help)." >&2
    fi
fi

echo
echo "APK(s):"
find "$PROJECT/app/build/outputs" -name '*.apk' 2>/dev/null | sed 's/^/  /' || true
