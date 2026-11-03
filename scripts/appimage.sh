#!/usr/bin/env bash
# Turn a staged desktop package into a single-file AppImage.
#
#   scripts/appimage.sh                          # stage the desktop package, then build
#   scripts/appimage.sh --stage release/<version>/linux/x64/desktop   # one already staged
#   scripts/appimage.sh --appdir-only            # build the AppDir, skip appimagetool
#
# The AppDir this assembles:
#
#   AppDir/AppRun                    <- redirects the writable paths, then execs romexplorer
#   AppDir/romexplorer.desktop       <- required at the top level by the AppImage spec
#   AppDir/romexplorer.png           <- ...as is an icon matching the desktop file's Icon=
#   AppDir/.DirIcon                  <- what file managers show for the image itself
#   AppDir/romexplorer, lib/, assets/ <- the desktop package, unchanged
#
# The package goes in exactly as scripts/package.sh staged it, so the bundle's relative
# rpaths keep resolving: romexplorer still finds lib/linux/<arch>/ beside itself. That
# is the reason this wraps package.sh rather than assembling its own tree.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$ROOT_DIR/scripts/dist"

STAGE="" ARCH="" VERSION="" OUT="" APPDIR_ONLY=0 APPIMAGETOOL="${APPIMAGETOOL:-}"

die() { echo "error: $*" >&2; exit 1; }
note() { echo "  $*"; }

usage() {
    sed -n '2,/^[^#]/p' "${BASH_SOURCE[0]}" | sed -n 's/^# \{0,1\}//p'
    cat <<'EOF'

Options:
  --stage DIR         a desktop package already staged by package.sh
                      (default: stage one now into release/<version>/linux/<arch>/desktop)
  --arch x64|arm64    (default: this host)
  --version V         (default: the workspace version)
  --out FILE          where to write the .AppImage (default: release/<version>/dist/<name>.AppImage)
  --appdir-only       assemble the AppDir and stop; useful when appimagetool cannot run
  --appimagetool P    use this appimagetool instead of downloading one
EOF
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --stage) STAGE="${2:-}"; shift 2 ;;
        --arch) ARCH="${2:-}"; shift 2 ;;
        --version) VERSION="${2:-}"; shift 2 ;;
        --out) OUT="${2:-}"; shift 2 ;;
        --appdir-only) APPDIR_ONLY=1; shift ;;
        --appimagetool) APPIMAGETOOL="${2:-}"; shift 2 ;;
        -h|--help) usage 0 ;;
        *) echo "unknown option: $1" >&2; usage 2 ;;
    esac
done

[[ "$(uname -s)" == "Linux" ]] || die "an AppImage can only be built on Linux"

if [[ -z "$ARCH" ]]; then
    case "$(uname -m)" in
        x86_64|amd64) ARCH="x64" ;;
        aarch64|arm64) ARCH="arm64" ;;
        *) die "can't tell what architecture this is; pass --arch" ;;
    esac
fi
# The app's own spelling on the left, the one appimagetool and the runtime use on the right.
case "$ARCH" in
    x64) AI_ARCH="x86_64" ;;
    arm64) AI_ARCH="aarch64" ;;
    *) die "--arch must be x64 or arm64" ;;
esac

if [[ -z "$VERSION" ]]; then
    VERSION="$(cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT_DIR/Cargo.toml" 2>/dev/null \
        | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="romexplorer"))' 2>/dev/null)" \
        || die "couldn't read the workspace version; pass --version"
fi
[[ -n "$VERSION" ]] || die "couldn't read the workspace version; pass --version"

echo "=== AppImage $VERSION for linux/$ARCH ==="

# --- the desktop package ---------------------------------------------------------------------
if [[ -z "$STAGE" ]]; then
    STAGE="$ROOT_DIR/release/$VERSION/linux/$ARCH/desktop"
    note "staging the desktop package first"
    "$ROOT_DIR/scripts/package.sh" --kind desktop --os linux --arch "$ARCH" --version "$VERSION" \
        --out "$STAGE" --no-archive >/dev/null || die "packaging the desktop build failed"
fi
[[ -d "$STAGE" ]] || die "no staged package at $STAGE"
[[ -f "$STAGE/romexplorer" ]] || die "$STAGE has no romexplorer -- is that a desktop package? (the server one has romexplorer_server instead)"
note "package: $STAGE"

# --- assemble the AppDir ----------------------------------------------------------------------
APPDIR="$ROOT_DIR/release/$VERSION/linux/$ARCH/appimage/romexplorer.AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR"
cp -a "$STAGE/." "$APPDIR/"
# install.sh is for the server; an AppImage installs nothing.
rm -f "$APPDIR/install.sh" "$APPDIR/romexplorer.conf.example"

install -m 755 "$DIST_DIR/AppRun" "$APPDIR/AppRun"
install -m 644 "$DIST_DIR/romexplorer.desktop" "$APPDIR/romexplorer.desktop"
note "AppRun + romexplorer.desktop"

# The committed icon, at the top level under the name the desktop file's Icon= key names,
# which is what appimagetool checks for. Required rather than generated: the artwork is
# rendered from scripts/dist/romexplorer.svg by scripts/render_icon.sh on a dev machine and
# committed, because no build host has a rasterizer. A missing one is a broken checkout, not
# a cue to invent a placeholder and ship it.
[[ -f "$DIST_DIR/romexplorer.png" ]] || die "no icon at scripts/dist/romexplorer.png -- regenerate it with scripts/render_icon.sh"
cp -f "$DIST_DIR/romexplorer.png" "$APPDIR/romexplorer.png"
note "icon: scripts/dist/romexplorer.png"
# Some file managers show .DirIcon for the image itself; a copy, not a symlink, because a
# dangling one inside a squashfs is a worse failure than a duplicated 30 KB.
cp -f "$APPDIR/romexplorer.png" "$APPDIR/.DirIcon"

# The freedesktop locations as well, for the case where the user installs the .desktop file
# into their menu: the menu looks up Icon=romexplorer in the theme, not inside the image.
mkdir -p "$APPDIR/usr/share/icons/hicolor/512x512/apps" "$APPDIR/usr/share/applications"
cp -f "$APPDIR/romexplorer.png" "$APPDIR/usr/share/icons/hicolor/512x512/apps/romexplorer.png"
cp -f "$APPDIR/romexplorer.desktop" "$APPDIR/usr/share/applications/romexplorer.desktop"

# --- gates ------------------------------------------------------------------------------------
# Everything appimagetool would reject, plus the two things it would happily accept and
# that would then fail only when a user double-clicks the result.
fail=0
[[ -x "$APPDIR/AppRun" ]] || { echo "error: AppRun is not executable" >&2; fail=1; }
sh -n "$APPDIR/AppRun" || { echo "error: AppRun does not parse as POSIX sh" >&2; fail=1; }
[[ -x "$APPDIR/romexplorer" ]] || { echo "error: romexplorer is not executable" >&2; fail=1; }
grep -q '^Icon=romexplorer$' "$APPDIR/romexplorer.desktop" || { echo "error: the desktop file's Icon= does not name romexplorer.png" >&2; fail=1; }
grep -q '^Categories=' "$APPDIR/romexplorer.desktop" || { echo "error: the desktop file needs Categories=" >&2; fail=1; }
[[ -f "$APPDIR/romexplorer.png" ]] || { echo "error: no icon at the AppDir root" >&2; fail=1; }
[[ -d "$APPDIR/lib/linux/$ARCH" ]] || { echo "error: the bundle has no lib/linux/$ARCH" >&2; fail=1; }
[[ "$fail" -eq 0 ]] || die "the AppDir is not valid; not building an image from it"
note "AppDir gates passed ($(du -sh "$APPDIR" | cut -f1))"

if [[ "$APPDIR_ONLY" -eq 1 ]]; then
    echo "AppDir ready at ${APPDIR#"$ROOT_DIR"/}"
    exit 0
fi

# --- appimagetool -------------------------------------------------------------------------------
if [[ -z "$APPIMAGETOOL" ]]; then
    if command -v appimagetool >/dev/null 2>&1; then
        APPIMAGETOOL="$(command -v appimagetool)"
    else
        cache="$ROOT_DIR/release/.tools"
        mkdir -p "$cache"
        APPIMAGETOOL="$cache/appimagetool-$AI_ARCH.AppImage"
        if [[ ! -x "$APPIMAGETOOL" ]]; then
            note "fetching appimagetool for $AI_ARCH"
            curl -fsSL --retry 4 --retry-delay 2 -o "$APPIMAGETOOL" \
                "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$AI_ARCH.AppImage" \
                || die "could not download appimagetool; pass --appimagetool PATH, or --appdir-only"
            chmod +x "$APPIMAGETOOL"
        fi
    fi
fi
note "appimagetool: $APPIMAGETOOL"

NAME="romexplorer-$VERSION-linux-$ARCH.AppImage"
OUT="${OUT:-$ROOT_DIR/release/$VERSION/dist/$NAME}"
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"

# appimagetool is itself an AppImage, so it wants FUSE. Where there is no /dev/fuse -- most
# containers, and most CI -- it can still run if it unpacks itself first, which is what
# APPIMAGE_EXTRACT_AND_RUN is for. Setting it always costs one extraction and removes a
# whole class of "works on my machine".
export APPIMAGE_EXTRACT_AND_RUN=1
export ARCH="$AI_ARCH"   # appimagetool reads this to pick the runtime it embeds

if ! "$APPIMAGETOOL" "$APPDIR" "$OUT"; then
    die "appimagetool failed (if it complains about FUSE, this already sets APPIMAGE_EXTRACT_AND_RUN=1; try --appdir-only and build the image on a host with FUSE)"
fi

[[ -f "$OUT" ]] || die "appimagetool reported success but produced no $OUT"
chmod +x "$OUT"
(cd "$(dirname "$OUT")" && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256")

echo
echo "built $(du -h "$OUT" | cut -f1) -> ${OUT#"$ROOT_DIR"/}"
echo "      ${OUT#"$ROOT_DIR"/}.sha256"
