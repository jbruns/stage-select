"""Writes the workbench's four WOFF2 fonts and their licences into crates/ui/assets/fonts.

Fetches each font at a pinned commit of its upstream repository, google/fonts or IBM/plex, checks its sha256, and
subsets it to Latin and Latin Extended with fontTools; text in other scripts falls back to the system font. Run it
from the repository's root after changing a pin or the ranges:

    uv run --with fonttools==4.68.0 --with brotli==1.2.1 python3 tools/fonts/subset.py

Its output is committed; nothing fetches a font at build or run time.
"""

import hashlib
import urllib.request
from pathlib import Path

from fontTools import subset

GOOGLE = "https://raw.githubusercontent.com/google/fonts/e774067705989f0bcb5e32eb0e8f76c24d679f92/ofl/chakrapetch"
PLEX = "https://raw.githubusercontent.com/IBM/plex/2e90fe366fdc27c12682db13e5aa20ded3d90908/packages"

# (output, source URL, the source's sha256)
FONTS = [
    ("ChakraPetch-Bold.woff2", f"{GOOGLE}/ChakraPetch-Bold.ttf",
     "65fbf76d95651697275e19db4d717c0e95a789ddd3476478b05292104db278a0"),
    ("IBMPlexSans-Regular.woff2", f"{PLEX}/plex-sans/fonts/complete/ttf/IBMPlexSans-Regular.ttf",
     "b47ebd72b5ce53b60f909033ff84d6fc12358cc147e2981f8c321ad3f36246b6"),
    ("IBMPlexSans-SemiBold.woff2", f"{PLEX}/plex-sans/fonts/complete/ttf/IBMPlexSans-SemiBold.ttf",
     "c51119b25f4a296da2b66edf3201b9aa1735091ed7efc1fca0313346f3851a4d"),
    ("IBMPlexMono-Regular.woff2", f"{PLEX}/plex-mono/fonts/complete/ttf/IBMPlexMono-Regular.ttf",
     "7c6fbddca4b700be918f5f6183d9bd4464fa427fe435f0b480d77fe2bb8c5a43"),
]

LICENCES = [
    ("ChakraPetch-OFL.txt", f"{GOOGLE}/OFL.txt",
     "13831d02389d917d22fcfa6c79f98f8acbf61d230add0e73a4d4dc5f3bfb9e56"),
    ("IBMPlex-OFL.txt", f"{PLEX}/plex-sans/LICENSE.txt",
     "7e6b2818edbd8f6a01ae80641cc8f16a51080d08fb4e532be3a0b6f74adb07da"),
]

# Basic Latin, Latin-1, Latin Extended-A to -D and Additional, IPA and phonetic extensions, with the punctuation,
# modifiers, combining marks, currency signs, arrows and symbols Latin text uses.
RANGES = [
    (0x0000, 0x036F), (0x1D00, 0x1DBF), (0x1E00, 0x1EFF), (0x2000, 0x20C0), (0x2100, 0x214F), (0x2190, 0x21FF),
    (0x2200, 0x22FF), (0x2C60, 0x2C7F), (0xA720, 0xA7FF), (0xFEFF, 0xFEFF), (0xFFFD, 0xFFFD),
]

OUT = Path("crates/ui/assets/fonts")


def fetch(url, sha256):
    data = urllib.request.urlopen(url).read()
    digest = hashlib.sha256(data).hexdigest()
    if digest != sha256:
        raise SystemExit(f"{url}: sha256 {digest}, pinned {sha256}")
    return data


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for name, url, sha256 in LICENCES:
        (OUT / name).write_bytes(fetch(url, sha256))
    unicodes = [c for start, end in RANGES for c in range(start, end + 1)]
    total = 0
    for name, url, sha256 in FONTS:
        source = OUT / f".{name}.ttf"
        source.write_bytes(fetch(url, sha256))
        options = subset.Options()
        options.flavor = "woff2"
        options.layout_features = ["*"]
        options.notdef_outline = True
        options.hinting = False
        options.desubroutinize = True
        try:
            font = subset.load_font(str(source), options)
            subsetter = subset.Subsetter(options)
            subsetter.populate(unicodes=unicodes)
            subsetter.subset(font)
            subset.save_font(font, str(OUT / name), options)
        finally:
            source.unlink()
        size = (OUT / name).stat().st_size
        total += size
        print(f"{name}: {size} bytes")
    print(f"total: {total} bytes")


if __name__ == "__main__":
    main()
