#!/usr/bin/env bash
# Turn a staged desktop package into a macOS .app bundle.
#
#   scripts/macos_app.sh                                  # stage the package, then bundle
#   scripts/macos_app.sh --stage release/<version>/macos/arm64/desktop
#   scripts/macos_app.sh --arch x64
#
# The layout, which is not a free choice:
#
#   romexplorer.app/Contents/MacOS/romexplorer          <- the executable
#   romexplorer.app/Contents/Resources/lib/, assets/    <- the rest of the package
#   romexplorer.app/Contents/Resources/romexplorer.icns
#   romexplorer.app/Contents/Info.plist
#
# A bundle is the one shape where the executable cannot sit beside lib/ and assets/: only
# code goes in Contents/MacOS/. `AppPaths::root_for_exe` (src/database/config.rs) therefore
# probes `Contents/Resources` explicitly -- Info.plist's LSEnvironment cannot say it instead,
# modern macOS ignores that key -- and scripts/package.sh emits the matching
# `@loader_path/../Resources/...` rpath for the libraries the GUI links.
#
# Signing: ad-hoc by default (`codesign -s -`), which needs no Apple account and no
# certificate. It makes the bundle a coherent signed unit -- without it only the executable
# carries the linker's own ad-hoc signature, and `codesign -v` on the bundle reports "code has
# no resources but signature indicates they must be present". It does NOT satisfy Gatekeeper:
# a downloaded copy is still quarantined and the first launch needs right-click > Open.
# Getting past that takes a Developer ID certificate (paid Apple Developer Program) passed as
# `--sign "Developer ID Application: ..."`, and then notarization, which this script leaves to
# whoever holds the credentials (`xcrun notarytool submit`, `xcrun stapler staple`).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$ROOT_DIR/scripts/dist"

STAGE="" ARCH="" VERSION="" OUT="" BUNDLE_ID="com.desertdoggy.romexplorer" SIGN_ID="-" ARCHIVE=1

die() { echo "error: $*" >&2; exit 1; }
note() { echo "  $*"; }

usage() {
    sed -n '2,/^[^#]/p' "${BASH_SOURCE[0]}" | sed -n 's/^# \{0,1\}//p'
    cat <<'EOF'

Options:
  --stage DIR         a desktop package already staged by package.sh
  --arch arm64|x64    (default: this host, or arm64 when cross-staging)
  --version V         (default: the workspace version)
  --out DIR           where to write romexplorer.app (default: release/<version>/macos/<arch>/app)
  --bundle-id ID      CFBundleIdentifier (default: com.desertdoggy.romexplorer)
  --sign IDENTITY     codesign identity (default: - , ad-hoc; "none" skips signing)
  --no-archive        build the bundle only; no zip in release/<version>/dist
EOF
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --stage) STAGE="${2:-}"; shift 2 ;;
        --arch) ARCH="${2:-}"; shift 2 ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --out) OUT="${2:-}"; shift 2 ;;
        --bundle-id) BUNDLE_ID="${2:-}"; shift 2 ;;
        --sign) SIGN_ID="${2:-}"; shift 2 ;;
        --no-archive) ARCHIVE=0; shift ;;
        -h|--help) usage 0 ;;
        *) echo "unknown option: $1" >&2; usage 2 ;;
    esac
done

if [[ -z "$ARCH" ]]; then
    case "$(uname -m)" in
        arm64|aarch64) ARCH="arm64" ;;
        x86_64|amd64) ARCH="x64" ;;
        *) ARCH="arm64" ;;
    esac
fi
case "$ARCH" in arm64|x64) ;; *) die "--arch must be arm64 or x64" ;; esac

if [[ -z "$VERSION" ]]; then
    VERSION="$(cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT_DIR/Cargo.toml" 2>/dev/null \
        | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="romexplorer"))' 2>/dev/null)" \
        || die "couldn't read the workspace version; pass --version"
fi
[[ -n "$VERSION" ]] || die "couldn't read the workspace version; pass --version"

echo "=== romexplorer.app $VERSION for macos/$ARCH ==="

if [[ -z "$STAGE" ]]; then
    STAGE="$ROOT_DIR/release/$VERSION/macos/$ARCH/desktop"
    note "staging the desktop package first"
    "$ROOT_DIR/scripts/package.sh" --kind desktop --os macos --arch "$ARCH" --version "$VERSION" \
        --out "$STAGE" --no-archive >/dev/null || die "packaging the desktop build failed"
fi
[[ -d "$STAGE" ]] || die "no staged package at $STAGE"
[[ -f "$STAGE/romexplorer" ]] || die "$STAGE has no romexplorer -- is that a desktop package?"
note "package: $STAGE"

OUT="${OUT:-$ROOT_DIR/release/$VERSION/macos/$ARCH/app}"
APP="$OUT/romexplorer.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp -a "$STAGE/romexplorer" "$APP/Contents/MacOS/romexplorer"
chmod +x "$APP/Contents/MacOS/romexplorer"
# Everything in the package but the executable: lib/, assets/, data/, LICENSE.
for entry in "$STAGE"/*; do
    [[ "$(basename "$entry")" == "romexplorer" ]] && continue
    cp -a "$entry" "$APP/Contents/Resources/"
done
note "Contents/MacOS/romexplorer + Contents/Resources/{lib,assets}"

[[ -f "$DIST_DIR/romexplorer.icns" ]] || die "no icon at scripts/dist/romexplorer.icns -- regenerate it with scripts/render_icon.sh"
cp -f "$DIST_DIR/romexplorer.icns" "$APP/Contents/Resources/romexplorer.icns"
note "Contents/Resources/romexplorer.icns"

# LSMinimumSystemVersion 11.0: the oldest macOS on Apple silicon, and the floor the arm64
# build targets anyway. CFBundleIconFile is spelled without the extension, which is what
# the Finder expects.
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>romexplorer</string>
    <key>CFBundleDisplayName</key><string>romexplorer</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key><string>romexplorer</string>
    <key>CFBundleIconFile</key><string>romexplorer</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
</dict>
</plist>
EOF
printf 'APPL????' > "$APP/Contents/PkgInfo"
note "Contents/Info.plist"

# --- sign -------------------------------------------------------------------------------
# Inside out: every Mach-O in the payload first, the bundle last, because the bundle's seal
# records each nested file's hash and re-signing one afterwards would break it. Not --deep,
# which Apple deprecated for signing and which would not carry the options to nested code.
# A real identity also gets the hardened runtime and a secure timestamp, both of which
# notarization requires; ad-hoc signing can take neither.
SIGNED=0
if [[ "$SIGN_ID" == "none" ]]; then
    note "signing skipped (--sign none)"
elif ! command -v codesign >/dev/null 2>&1; then
    note "no codesign here; the bundle is left unsigned (build it on macOS to sign it)"
else
    sign_args=(--force --sign "$SIGN_ID")
    [[ "$SIGN_ID" == "-" ]] || sign_args+=(--options runtime --timestamp)
    n=0
    while IFS= read -r -d '' f; do
        file "$f" | grep -q Mach-O || continue
        codesign "${sign_args[@]}" "$f" 2>/dev/null || die "codesign failed on ${f#"$APP"/}"
        n=$((n+1))
    done < <(find "$APP/Contents/Resources" -type f -print0)
    codesign "${sign_args[@]}" "$APP" || die "codesign failed on the bundle"
    SIGNED=1
    note "signed $n nested binaries + the bundle ($([[ "$SIGN_ID" == "-" ]] && echo ad-hoc || echo "$SIGN_ID"))"
fi

# --- gates ------------------------------------------------------------------------------
# Everything that would otherwise fail only when someone double-clicks the bundle.
fail=0
[[ -x "$APP/Contents/MacOS/romexplorer" ]] || { echo "error: Contents/MacOS/romexplorer is not executable" >&2; fail=1; }
[[ -d "$APP/Contents/Resources/lib/macos/$ARCH" ]] || { echo "error: no Resources/lib/macos/$ARCH -- AppPaths::detect would not find the app root" >&2; fail=1; }
[[ -f "$APP/Contents/Resources/romexplorer.icns" ]] || { echo "error: the icns is missing" >&2; fail=1; }

# The plist must parse, and CFBundleExecutable/CFBundleIconFile must name files that exist:
# a typo in either is a bundle that launches to nothing or shows a blank Dock tile.
python3 - "$APP" <<'PY' || fail=1
import plistlib, sys, pathlib
app = pathlib.Path(sys.argv[1])
p = plistlib.loads((app / "Contents/Info.plist").read_bytes())
ok = True
exe = app / "Contents/MacOS" / p["CFBundleExecutable"]
if not exe.is_file():
    print(f"error: CFBundleExecutable names {p['CFBundleExecutable']}, which is not in Contents/MacOS", file=sys.stderr); ok = False
icon = app / "Contents/Resources" / (p["CFBundleIconFile"] + ".icns")
if not icon.is_file():
    print(f"error: CFBundleIconFile names {p['CFBundleIconFile']}, but {icon.name} is not in Contents/Resources", file=sys.stderr); ok = False
sys.exit(0 if ok else 1)
PY

# The whole reason package.sh emits a second rpath on macOS. Only checkable there, where otool is.
if command -v otool >/dev/null 2>&1; then
    rpaths="$(otool -l "$APP/Contents/MacOS/romexplorer" 2>/dev/null | awk '/LC_RPATH/{f=1} f&&/path /{print $2; f=0}')"
    if ! printf '%s\n' "$rpaths" | grep -q '\.\./Resources/lib/'; then
        echo "error: the executable has no @loader_path/../Resources/lib rpath, so the bundled libraries will not resolve." >&2
        echo "       Rebuild through scripts/package.sh (it sets this); rpaths present: $(printf '%s ' $rpaths)" >&2
        fail=1
    fi
    bad="$(otool -L "$APP/Contents/MacOS/romexplorer" 2>/dev/null | tail -n +2 | awk '{print $1}' \
        | grep -vE '^(@loader_path|@rpath|@executable_path|/usr/lib/|/System/)' || true)"
    [[ -z "$bad" ]] || { echo "error: the executable loads $bad from an absolute path" >&2; fail=1; }
else
    note "no otool here; the rpath and absolute-load checks are skipped (run this on macOS to get them)"
fi

if [[ "$SIGNED" -eq 1 ]]; then
    codesign --verify --deep --strict "$APP" 2>&1 || { echo "error: the signed bundle does not verify" >&2; fail=1; }
fi

[[ "$fail" -eq 0 ]] || die "the bundle is not valid; not shipping it"
note "bundle gates passed ($(du -sh "$APP" | cut -f1))"

# --- archive ----------------------------------------------------------------------------
# A .app is a directory, so it travels as a zip -- the form macOS users expect and Finder
# unpacks on a double-click. ditto rather than zip(1) where there is one: it is what Apple
# documents for a signed bundle (notarytool takes exactly this), and it keeps the symlinks,
# modes and extended attributes the signature's seal covers.
if [[ "$ARCHIVE" -eq 1 ]]; then
    DIST="$ROOT_DIR/release/$VERSION/dist"
    mkdir -p "$DIST"
    zip_out="$DIST/romexplorer-app-$VERSION-macos-$ARCH.zip"
    rm -f "$zip_out"
    if command -v ditto >/dev/null 2>&1; then
        ditto -c -k --keepParent "$APP" "$zip_out" || die "archiving the bundle failed"
    elif command -v zip >/dev/null 2>&1; then
        (cd "$OUT" && zip -qry "$zip_out" romexplorer.app) || die "archiving the bundle failed"
    else
        die "neither ditto nor zip here to archive the bundle (or pass --no-archive)"
    fi
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$DIST" && sha256sum "$(basename "$zip_out")" > "$(basename "$zip_out").sha256")
    else
        (cd "$DIST" && shasum -a 256 "$(basename "$zip_out")" > "$(basename "$zip_out").sha256")
    fi
    note "archived $(du -h "$zip_out" | cut -f1) -> ${zip_out#"$ROOT_DIR"/}"
fi

echo
echo "built ${APP#"$ROOT_DIR"/}"
if [[ "$SIGN_ID" == "-" || "$SIGNED" -eq 0 ]]; then
    echo "Not Developer ID signed: macOS quarantines it on download, so the first launch needs right-click > Open."
else
    echo "Signed with $SIGN_ID; notarize it before distributing (xcrun notarytool submit, then xcrun stapler staple)."
fi
