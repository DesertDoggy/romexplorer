#!/usr/bin/env bash
# Turn a staged desktop, chdlite or rommusic package into a single-file AppImage.
#
#   scripts/appimage.sh                          # stage the desktop package, then build
#   scripts/appimage.sh --stage release/<version>/linux/x64/desktop   # one already staged
#   scripts/appimage.sh --kind chdlite           # chdlite: the window, with the console tools inside
#   scripts/appimage.sh --kind rommusic          # rommusic: the same
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
# chdlite and rommusic are the same shape with their own names (chdlite.desktop, chdlite.png),
# their own versions (chdlite/Cargo.toml, rommusic-cli/Cargo.toml) and their own trees
# (release/chdlite/<version>/), and scripts/dist/AppRun.tool as the entry point: it starts
# the window, or a console tool named as the first argument (chdlite.AppImage chdcomp ...).
#
# The package goes in exactly as scripts/package.sh staged it, so the bundle's relative
# rpaths keep resolving: romexplorer still finds lib/linux/<arch>/ beside itself. That
# is the reason this wraps package.sh rather than assembling its own tree.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$ROOT_DIR/scripts/dist"

KIND="desktop" STAGE="" ARCH="" VERSION="" OUT="" APPDIR_ONLY=0 APPIMAGETOOL="${APPIMAGETOOL:-}"

die() { echo "error: $*" >&2; exit 1; }
note() { echo "  $*"; }

usage() {
    sed -n '2,/^[^#]/p' "${BASH_SOURCE[0]}" | sed -n 's/^# \{0,1\}//p'
    cat <<'EOF'

Options:
  --kind K            desktop (romexplorer), chdlite or rommusic   (default: desktop)
  --stage DIR         a package of that kind already staged by package.sh
                      (default: stage one now: release/<version>/linux/<arch>/desktop, or
                      release/chdlite/<version>/linux/<arch> and the rommusic equivalent)
  --arch x64|arm64    (default: this host)
  --version V         (default: the kind's own version: the workspace, chdlite/Cargo.toml
                      or rommusic-cli/Cargo.toml)
  --out FILE          where to write the .AppImage (default: <tree>/dist/<name>.AppImage)
  --appdir-only       assemble the AppDir and stop; useful when appimagetool cannot run
  --appimagetool P    use this appimagetool instead of downloading one
EOF
    exit "${1:-0}"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --kind) KIND="${2:-}"; shift 2 ;;
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

# Per kind: the app's name (desktop file, icon, AppImage), the executable AppRun starts, the
# console tools beside it, the committed icon, the Cargo package that carries the version,
# and the release tree everything lands in.
case "$KIND" in
    desktop)
        APP=romexplorer GUI=romexplorer TOOLS="" ICON="$DIST_DIR/romexplorer.png" PKG=romexplorer
        ICON_HINT="regenerate it with scripts/render_icon.sh" ;;
    chdlite)
        APP=chdlite GUI=chdlite-gui TOOLS="chdlite chdcomp chdread chdhash" ICON="$ROOT_DIR/chdlite/assets/icon.png" PKG=chdlite
        ICON_HINT="it is committed with chdlite" ;;
    rommusic)
        APP=rommusic GUI=rommusic-gui TOOLS="rommusic" ICON="$ROOT_DIR/rommusic-cli/assets/icon.png" PKG=rommusic-cli
        ICON_HINT="it is committed with rommusic-cli" ;;
    *) die "--kind must be desktop, chdlite or rommusic" ;;
esac

if [[ -z "$VERSION" ]]; then
    VERSION="$(cargo metadata --no-deps --format-version 1 --manifest-path "$ROOT_DIR/Cargo.toml" 2>/dev/null \
        | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]==sys.argv[1]))' "$PKG" 2>/dev/null)" \
        || die "couldn't read the $PKG version; pass --version"
fi
[[ -n "$VERSION" ]] || die "couldn't read the $PKG version; pass --version"

# The AppDir goes beside the staged package, never inside it: chdlite's and rommusic's
# packages are staged straight into <tree>/linux/<arch>/, with no kind level to sit under.
if [[ "$KIND" == "desktop" ]]; then
    TREE="$ROOT_DIR/release/$VERSION"
    DEFAULT_STAGE="$TREE/linux/$ARCH/desktop"
    APPDIR="$TREE/linux/$ARCH/appimage/$APP.AppDir"
else
    TREE="$ROOT_DIR/release/$APP/$VERSION"
    DEFAULT_STAGE="$TREE/linux/$ARCH"
    APPDIR="$TREE/bundles/linux/$ARCH/$APP.AppDir"
fi

echo "=== $APP AppImage $VERSION for linux/$ARCH ==="

# --- the package ------------------------------------------------------------------------------
if [[ -z "$STAGE" ]]; then
    STAGE="$DEFAULT_STAGE"
    note "staging the $KIND package first"
    "$ROOT_DIR/scripts/package.sh" --kind "$KIND" --os linux --arch "$ARCH" --version "$VERSION" \
        --out "$STAGE" --no-archive >/dev/null || die "packaging the $KIND build failed"
fi
[[ -d "$STAGE" ]] || die "no staged package at $STAGE"
[[ -f "$STAGE/$GUI" ]] || die "$STAGE has no $GUI -- is that a $KIND package? (the server one has romexplorer_server instead)"
note "package: $STAGE"

# --- assemble the AppDir ----------------------------------------------------------------------
rm -rf "$APPDIR"
mkdir -p "$APPDIR"
cp -a "$STAGE/." "$APPDIR/"
# install.sh is for the server; an AppImage installs nothing.
rm -f "$APPDIR/install.sh" "$APPDIR/romexplorer.conf.example"

# AppRun: romexplorer's own, or the tools' one, filled in below once the icon size is known.
if [[ "$KIND" == "desktop" ]]; then
    install -m 755 "$DIST_DIR/AppRun" "$APPDIR/AppRun"
fi
install -m 644 "$DIST_DIR/$APP.desktop" "$APPDIR/$APP.desktop"
note "AppRun + $APP.desktop"

# The committed icon, at the top level under the name the desktop file's Icon= key names,
# which is what appimagetool checks for. Required rather than generated: romexplorer's is
# rendered from scripts/dist/romexplorer.svg by scripts/render_icon.sh on a dev machine and
# committed, because no build host has a rasterizer. A missing one is a broken checkout, not
# a cue to invent a placeholder and ship it.
[[ -f "$ICON" ]] || die "no icon at ${ICON#"$ROOT_DIR"/} -- $ICON_HINT"
cp -f "$ICON" "$APPDIR/$APP.png"
note "icon: ${ICON#"$ROOT_DIR"/}"
# Some file managers show .DirIcon for the image itself; a copy, not a symlink, because a
# dangling one inside a squashfs is a worse failure than a duplicated 30 KB.
cp -f "$APPDIR/$APP.png" "$APPDIR/.DirIcon"

# The freedesktop locations as well, for the case where the user installs the .desktop file
# into their menu: the menu looks up Icon=<app> in the theme, not inside the image. The size
# directory has to match the picture (chdlite's is 256 px, the others 512).
icon_px="$(python3 -c 'import struct,sys; d=open(sys.argv[1],"rb").read(24); print(struct.unpack(">I",d[16:20])[0])' "$ICON")"
if [[ "$KIND" != "desktop" ]]; then
    sed -e "s|@APP@|$APP|g" -e "s|@GUI@|$GUI|g" -e "s|@TOOLS@|$TOOLS|g" -e "s|@ICON_SIZE@|$icon_px|g" \
        "$DIST_DIR/AppRun.tool" > "$APPDIR/AppRun"
    chmod 755 "$APPDIR/AppRun"
fi
mkdir -p "$APPDIR/usr/share/icons/hicolor/${icon_px}x${icon_px}/apps" "$APPDIR/usr/share/applications"
cp -f "$APPDIR/$APP.png" "$APPDIR/usr/share/icons/hicolor/${icon_px}x${icon_px}/apps/$APP.png"
cp -f "$APPDIR/$APP.desktop" "$APPDIR/usr/share/applications/$APP.desktop"

# --- gates ------------------------------------------------------------------------------------
# Everything appimagetool would reject, plus the two things it would happily accept and
# that would then fail only when a user double-clicks the result.
fail=0
[[ -x "$APPDIR/AppRun" ]] || { echo "error: AppRun is not executable" >&2; fail=1; }
sh -n "$APPDIR/AppRun" || { echo "error: AppRun does not parse as POSIX sh" >&2; fail=1; }
grep -q '@[A-Z][A-Z]*@' "$APPDIR/AppRun" && { echo "error: AppRun still has an unfilled @...@ value" >&2; fail=1; }
for exe in $GUI $TOOLS; do
    [[ -x "$APPDIR/$exe" ]] || { echo "error: $exe is not executable" >&2; fail=1; }
done
grep -q "^Icon=$APP\$" "$APPDIR/$APP.desktop" || { echo "error: the desktop file's Icon= does not name $APP.png" >&2; fail=1; }
grep -q '^Categories=' "$APPDIR/$APP.desktop" || { echo "error: the desktop file needs Categories=" >&2; fail=1; }
[[ -f "$APPDIR/$APP.png" ]] || { echo "error: no icon at the AppDir root" >&2; fail=1; }
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
        # The tool runs here, so it is this host's build; the image's arch is the runtime it
        # embeds (below). An aarch64 tool on an x64 host fails with "Exec format error".
        case "$(uname -m)" in
            x86_64|amd64) TOOL_ARCH="x86_64" ;;
            aarch64|arm64) TOOL_ARCH="aarch64" ;;
            *) die "no appimagetool for host $(uname -m); pass --appimagetool PATH" ;;
        esac
        cache="$ROOT_DIR/release/.tools"
        mkdir -p "$cache"
        APPIMAGETOOL="$cache/appimagetool-$TOOL_ARCH.AppImage"
        if [[ ! -x "$APPIMAGETOOL" ]]; then
            note "fetching appimagetool for $TOOL_ARCH"
            curl -fsSL --retry 4 --retry-delay 2 -o "$APPIMAGETOOL" \
                "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-$TOOL_ARCH.AppImage" \
                || die "could not download appimagetool; pass --appimagetool PATH, or --appdir-only"
            chmod +x "$APPIMAGETOOL"
        fi
    fi
fi
note "appimagetool: $APPIMAGETOOL"

NAME="$APP-$VERSION-linux-$ARCH.AppImage"
OUT="${OUT:-$TREE/dist/$NAME}"
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"

# appimagetool is itself an AppImage, so it wants FUSE. Where there is no /dev/fuse -- most
# containers, and most CI -- it can still run if it unpacks itself first, which is what
# APPIMAGE_EXTRACT_AND_RUN is for. Setting it always costs one extraction and removes a
# whole class of "works on my machine".
export APPIMAGE_EXTRACT_AND_RUN=1
export ARCH="$AI_ARCH"   # appimagetool reads this to pick the runtime it embeds

# The runtime is the image's own arch, not the tool's. Fetched once and handed over, so a
# cross build does not depend on appimagetool finding the right one by itself.
runtime="$ROOT_DIR/release/.tools/runtime-$AI_ARCH"
if [[ ! -s "$runtime" ]]; then
    mkdir -p "$(dirname "$runtime")"
    note "fetching the AppImage runtime for $AI_ARCH"
    curl -fsSL --retry 4 --retry-delay 2 -o "$runtime" \
        "https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-$AI_ARCH" \
        || { rm -f "$runtime"; die "could not download the $AI_ARCH AppImage runtime; try --appdir-only"; }
fi

if ! "$APPIMAGETOOL" --runtime-file "$runtime" "$APPDIR" "$OUT"; then
    die "appimagetool failed (if it complains about FUSE, this already sets APPIMAGE_EXTRACT_AND_RUN=1; try --appdir-only and build the image on a host with FUSE)"
fi

[[ -f "$OUT" ]] || die "appimagetool reported success but produced no $OUT"
chmod +x "$OUT"
(cd "$(dirname "$OUT")" && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256")

echo
echo "built $(du -h "$OUT" | cut -f1) -> ${OUT#"$ROOT_DIR"/}"
echo "      ${OUT#"$ROOT_DIR"/}.sha256"
