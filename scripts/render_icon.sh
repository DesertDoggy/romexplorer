#!/usr/bin/env bash
# Render scripts/dist/romexplorer.svg into the bitmaps the packaging scripts ship.
#
#   scripts/render_icon.sh
#
# A DEV-MACHINE step, deliberately not part of any build. It needs a Chromium to rasterize
# the SVG, and no build host here or in CI has one (nor Pillow, ImageMagick or rsvg) which is
# exactly why its outputs are committed:
#
#   scripts/dist/romexplorer.png    512px, what appimage.sh and the .desktop entry use
#   scripts/dist/romexplorer.icns   the macOS .app bundle's CFBundleIconFile
#   scripts/dist/romexplorer.ico    Windows
#
# Run it after editing the SVG and commit what changes. If it is never run the committed
# bitmaps simply stay as they are; nothing breaks, they just stop matching the source.
#
# Chromium is found automatically: PLAYWRIGHT_BROWSERS_PATH (this repo's dev containers ship
# one), then the usual names on PATH. Override with CHROME=/path/to/chrome.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST="$ROOT_DIR/scripts/dist"
SVG="$DIST/romexplorer.svg"

die() { echo "error: $*" >&2; exit 1; }
note() { echo "  $*"; }

[[ -f "$SVG" ]] || die "no $SVG to render"

CHROME="${CHROME:-}"
if [[ -z "$CHROME" ]]; then
    for c in "${PLAYWRIGHT_BROWSERS_PATH:-/opt/pw-browsers}"/chromium-*/chrome-linux/chrome \
             /opt/pw-browsers/chromium/chrome-linux/chrome; do
        [[ -x "$c" ]] && CHROME="$c" && break
    done
fi
if [[ -z "$CHROME" ]]; then
    for c in chromium chromium-browser google-chrome google-chrome-stable; do
        command -v "$c" >/dev/null 2>&1 && CHROME="$(command -v "$c")" && break
    done
fi
[[ -n "$CHROME" ]] || die "no Chromium found; set CHROME=/path/to/chrome"
note "chromium: $CHROME"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Every size the three container formats want, rendered from the vector each time rather
# than downscaled from one bitmap: the strokes stay crisp that way.
SIZES=(16 24 32 48 64 128 256 512 1024)

for s in "${SIZES[@]}"; do
    # An HTML wrapper rather than the .svg directly: it is the only way to pin an exact
    # pixel size with a transparent backdrop and no scrollbars.
    cat > "$TMP/$s.html" <<HTML
<!doctype html><meta charset="utf-8">
<style>html,body{margin:0;padding:0;background:transparent;overflow:hidden}
svg{display:block;width:${s}px;height:${s}px}</style>
$(sed '1{/<?xml/d;}' "$SVG" | sed -E 's/<svg ([^>]*)width="512" height="512"/<svg \1/')
HTML
    # The viewport is deliberately TALLER than the icon and cropped back to a square below.
    # A window whose height equals the content's gets clipped: at --window-size=512,512
    # chromium renders only to row 424 and leaves the rest transparent, which silently cost
    # the entire wordmark. Anything taller renders all 512 rows.
    "$CHROME" --headless --disable-gpu --no-sandbox --hide-scrollbars \
        --default-background-color=00000000 --virtual-time-budget=2000 \
        --screenshot="$TMP/shot-$s.png" --window-size="$s,$((s + 240))" \
        "file://$TMP/$s.html" >/dev/null 2>&1
    [[ -f "$TMP/shot-$s.png" ]] || die "chromium produced no output at ${s}px"
done
note "rendered ${#SIZES[@]} sizes"

# Crop each render back to a square, then pack the containers. .icns and .ico both accept
# embedded PNG data, so neither needs macOS or Windows tooling, just the standard library.
python3 - "$TMP" "$DIST" <<'PYEOF'
import struct, sys, zlib, pathlib
tmp, dist = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])

def read_png(path):
    d = path.read_bytes(); i = 8; idat = b""; W = H = 0
    while i < len(d):
        ln = struct.unpack(">I", d[i:i+4])[0]; tag = d[i+4:i+8]
        if tag == b"IHDR":
            W, H, bits, colour = struct.unpack(">IIBB", d[i+8:i+18])
            assert (bits, colour) == (8, 6), f"expected 8-bit RGBA, got {bits}/{colour}"
        elif tag == b"IDAT":
            idat += d[i+8:i+8+ln]
        i += 12 + ln
    raw = zlib.decompress(idat); stride = 1 + W*4
    rows, prev = [], bytearray(W*4)
    for y in range(H):                       # undo PNG's per-row adaptive filtering
        ft = raw[y*stride]; line = bytearray(raw[y*stride+1:(y+1)*stride])
        for x in range(len(line)):
            a = line[x-4] if x >= 4 else 0
            b = prev[x]
            c = prev[x-4] if x >= 4 else 0
            if   ft == 1: line[x] = (line[x] + a) & 255
            elif ft == 2: line[x] = (line[x] + b) & 255
            elif ft == 3: line[x] = (line[x] + (a + b)//2) & 255
            elif ft == 4:
                p = a + b - c; pa, pb, pc = abs(p-a), abs(p-b), abs(p-c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else b if pb <= pc else c)) & 255
        rows.append(bytes(line)); prev = line
    return W, H, rows

def write_png(path, W, rows):
    raw = b"".join(b"\x00" + r for r in rows)
    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)
    path.write_bytes(b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", W, len(rows), 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b""))

png = {}
for shot in sorted(tmp.glob("shot-*.png")):
    size = int(shot.stem.split("-")[1])
    W, H, rows = read_png(shot)
    assert W == size and H >= size, f"{shot.name}: {W}x{H} cannot yield a {size}px square"
    # The clipping this script works around was invisible until it ate the wordmark, so it
    # is asserted rather than trusted: the artwork must reach the bottom of every square.
    ink = max((y for y in range(H) if any(rows[y][x*4+3] > 8 for x in range(W))), default=-1)
    assert ink >= size - 2, f"{size}px render is clipped: last row with ink is {ink}, want {size-1}"
    out = tmp / f"icon-{size}.png"
    write_png(out, W, rows[:size])
    png[size] = out.read_bytes()

(dist / "romexplorer.png").write_bytes(png[512])
print("  romexplorer.png (512)")

icns_types = [(b"ic11", 32), (b"ic12", 64), (b"ic07", 128), (b"ic08", 256), (b"ic09", 512), (b"ic10", 1024)]
body = b"".join(t + struct.pack(">I", len(png[s]) + 8) + png[s] for t, s in icns_types)
(dist / "romexplorer.icns").write_bytes(b"icns" + struct.pack(">I", len(body) + 8) + body)
print(f"  romexplorer.icns ({len(icns_types)} sizes)")

sizes = [16, 24, 32, 48, 64, 128, 256]
offset = 6 + 16 * len(sizes); entries = b""
for s in sizes:
    # 0 in the width/height byte is the format's way of spelling 256.
    entries += struct.pack("<BBBBHHII", s if s < 256 else 0, s if s < 256 else 0,
                           0, 0, 1, 32, len(png[s]), offset)
    offset += len(png[s])
(dist / "romexplorer.ico").write_bytes(
    struct.pack("<HHH", 0, 1, len(sizes)) + entries + b"".join(png[s] for s in sizes))
print(f"  romexplorer.ico ({len(sizes)} sizes)")
PYEOF

echo
echo "wrote:"
for f in romexplorer.png romexplorer.icns romexplorer.ico; do
    printf '  %-20s %s\n' "$f" "$(du -h "$DIST/$f" | cut -f1)"
done
echo "Commit these alongside the .svg they came from."
