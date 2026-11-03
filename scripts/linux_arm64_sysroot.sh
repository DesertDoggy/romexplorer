#!/usr/bin/env bash
# Unpacks the arm64 WebKitGTK development files into a private directory, so the desktop GUI
# can be cross-built for linux/arm64 on an x64 machine.
#
# Why not just `apt install libwebkit2gtk-4.1-dev:arm64`: the -dev packages are not
# co-installable across architectures, so apt removes the x64 ones to make room -- and the
# x64 desktop build on the same machine stops working. Nothing here is installed: the .debs
# are downloaded and extracted with dpkg-deb, no root needed, and only scripts/package.sh's
# linux/arm64 build ever looks at the result.
#
#   scripts/linux_arm64_sysroot.sh            # build it (about 160 MB of downloads)
#   scripts/linux_arm64_sysroot.sh --force    # rebuild from scratch
#
# Needs arm64 as a foreign architecture with package lists fetched, once:
#   sudo dpkg --add-architecture arm64 && sudo apt update
#
# Output: tests/output/sysroot/linux-arm64/ (git-ignored), or $LINUX_ARM64_SYSROOT.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SYSROOT="${LINUX_ARM64_SYSROOT:-$ROOT_DIR/tests/output/sysroot/linux-arm64}"
# What the GUI's -sys crates ask pkg-config (or the linker) for.
WANT=(libwebkit2gtk-4.1-dev:arm64 libxdo-dev:arm64)
FORCE=0
[[ "${1:-}" == "--force" ]] && FORCE=1

die() { echo "error: $*" >&2; exit 1; }
for t in apt-cache apt-get dpkg-deb; do command -v "$t" >/dev/null 2>&1 || die "$t not found (this script is for Debian/Ubuntu hosts)"; done
dpkg --print-foreign-architectures | grep -qx arm64 \
    || die "arm64 is not a foreign architecture here: sudo dpkg --add-architecture arm64 && sudo apt update"
apt-cache show "${WANT[0]}" >/dev/null 2>&1 \
    || die "apt knows no ${WANT[0]} -- sudo apt update (and check your sources carry arm64)"

STAMP="$SYSROOT/.complete"
if [[ -f "$STAMP" && "$FORCE" -eq 0 ]]; then
    echo "sysroot already built: ${SYSROOT#$ROOT_DIR/} (--force to rebuild)"
    exit 0
fi

# The dependency closure, minus what does not belong in a link-time sysroot:
#   * only lib*:arm64 -- the recursion also walks into icon themes, dictionaries and daemons;
#   * plus what the .pc files require that is not named lib*: every other -dev:arm64
#     (zlib1g-dev, uuid-dev, krb5-multidev, ...), zlib1g itself, and shared-mime-info for its .pc, which
#     gdk-pixbuf-2.0 requires. Without them pkg-config fails every GTK -sys crate;
#   * plus the architecture-independent header packages (x11proto-dev and friends), which
#     apt lists without an arch suffix;
#   * never libc / libgcc / libstdc++: the cross compiler brings its own, and a second
#     libc.so linker script on the search path points the link at files that are not there.
mapfile -t PKGS < <(
    apt-cache depends --recurse --no-recommends --no-suggests --no-conflicts --no-breaks \
        --no-replaces --no-enhances "${WANT[@]}" 2>/dev/null \
    | grep -E '^[a-z0-9]' | sort -u \
    | grep -E '^lib.*:arm64$|dev:arm64$|^(zlib1g|shared-mime-info):arm64$|^[^:]*(-dev|-protocols)$' \
    | grep -vE '^(libc6|libc6-dev|libc-dev-bin|libgcc-|libstdc\+\+|libcrypt-dev|linux-libc-dev)'
)
[[ ${#PKGS[@]} -gt 0 ]] || die "apt-cache returned no packages"

DEBS="$SYSROOT.debs"
rm -rf "$SYSROOT"
mkdir -p "$SYSROOT" "$DEBS"
echo "downloading ${#PKGS[@]} packages into ${DEBS#$ROOT_DIR/}"
# Virtual packages in the closure have nothing to download; apt-get says so and carries on
# only when asked one at a time, so the bulk call is retried per package on failure.
if ! (cd "$DEBS" && apt-get download "${PKGS[@]}" >/dev/null 2>&1); then
    for p in "${PKGS[@]}"; do
        (cd "$DEBS" && apt-get download "$p" >/dev/null 2>&1) || echo "  skipped $p (virtual, or not available for arm64)"
    done
fi

n=0
for deb in "$DEBS"/*.deb; do
    [[ -f "$deb" ]] || continue
    dpkg-deb -x "$deb" "$SYSROOT"
    n=$((n + 1))
done
[[ "$n" -gt 0 ]] || die "nothing was downloaded"

[[ -f "$SYSROOT/usr/lib/aarch64-linux-gnu/pkgconfig/webkit2gtk-4.1.pc" ]] \
    || die "the sysroot has no webkit2gtk-4.1.pc -- the download was incomplete"
date -u +%Y-%m-%dT%H:%M:%SZ > "$STAMP"
echo "unpacked $n packages -> ${SYSROOT#$ROOT_DIR/} ($(du -sh "$SYSROOT" | cut -f1))"
echo "the .debs in ${DEBS#$ROOT_DIR/} can be deleted"
