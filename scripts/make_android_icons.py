#!/usr/bin/env python3
"""Makes an Android app's launcher icon resources from its desktop icon.

dx (0.7) ignores Dioxus.toml's `icon` on Android and always writes its own launcher icon
(the Android robot) into the Gradle project. The Android build scripts
(scripts/build_chdlite_android.sh, scripts/build_rommusic_android.sh) copy the resources
this makes over dx's, from <crate>/android/res/:

  mipmap-<dpi>/ic_launcher.png             the whole icon, for Android 7 and older
  mipmap-<dpi>/ic_launcher_foreground.png  the icon centred in the adaptive icon's 108 dp
  mipmap-anydpi-v26/ic_launcher.xml        the adaptive icon (Android 8+): that foreground
                                            over a plain background colour
  values/ic_launcher_background.xml        the background colour

Run it again after an icon changes (needs Pillow):

  python3 scripts/make_android_icons.py chdlite/assets/chdlite.icns chdlite/android/res
  python3 scripts/make_android_icons.py rommusic-cli/assets/icon.png rommusic-cli/android/res
"""

import argparse
from pathlib import Path

from PIL import Image

# dp -> px factor for each density.
DENSITIES = {"mdpi": 1, "hdpi": 1.5, "xhdpi": 2, "xxhdpi": 3, "xxxhdpi": 4}
LEGACY_DP = 48
ADAPTIVE_DP = 108
# How wide the icon's drawn part is inside the 108 dp foreground. Launchers mask to a circle of
# 72 dp at most and 66 dp is always shown; 56 dp keeps a square icon's corners (where both
# icons have their name badge) inside most masks.
CONTENT_DP = 56


def load(path: Path) -> Image.Image:
    im = Image.open(path)
    if path.suffix == ".icns":
        # The largest size the file holds.
        im.size = max(im.info["sizes"])[:2]
        im.load()
    im = im.convert("RGBA")
    return im.crop(im.getbbox())


def fit(im: Image.Image, box: int) -> Image.Image:
    scale = box / max(im.size)
    return im.resize((max(1, round(im.width * scale)), max(1, round(im.height * scale))), Image.LANCZOS)


def centred(im: Image.Image, side: int) -> Image.Image:
    canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    canvas.alpha_composite(im, ((side - im.width) // 2, (side - im.height) // 2))
    return canvas


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("icon", type=Path, help="the desktop icon (.png or .icns)")
    ap.add_argument("res", type=Path, help="the res/ folder to write")
    ap.add_argument("--background", default="#1E2228", help="adaptive icon background colour")
    args = ap.parse_args()

    icon = load(args.icon)
    for dpi, k in DENSITIES.items():
        out = args.res / f"mipmap-{dpi}"
        out.mkdir(parents=True, exist_ok=True)
        legacy = round(LEGACY_DP * k)
        # 2 dp of margin, as Android's own legacy icons have.
        centred(fit(icon, round((LEGACY_DP - 4) * k)), legacy).save(out / "ic_launcher.png", optimize=True)
        centred(fit(icon, round(CONTENT_DP * k)), round(ADAPTIVE_DP * k)).save(
            out / "ic_launcher_foreground.png", optimize=True
        )

    anydpi = args.res / "mipmap-anydpi-v26"
    anydpi.mkdir(parents=True, exist_ok=True)
    (anydpi / "ic_launcher.xml").write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n'
        "<!-- Made by scripts/make_android_icons.py. -->\n"
        '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
        '    <background android:drawable="@color/ic_launcher_background" />\n'
        '    <foreground android:drawable="@mipmap/ic_launcher_foreground" />\n'
        "</adaptive-icon>\n",
        newline="\n",
    )
    values = args.res / "values"
    values.mkdir(parents=True, exist_ok=True)
    (values / "ic_launcher_background.xml").write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n'
        "<!-- Made by scripts/make_android_icons.py. -->\n"
        "<resources>\n"
        f'    <color name="ic_launcher_background">{args.background}</color>\n'
        "</resources>\n",
        newline="\n",
    )


if __name__ == "__main__":
    main()
