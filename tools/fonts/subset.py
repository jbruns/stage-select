"""Writes the workbench's four WOFF2 fonts and their licences into crates/ui/assets/fonts.

Fetches each font at a pinned commit of its upstream repository, google/fonts or IBM/plex, checks its sha256, and
subsets it to Latin and Latin Extended with fontTools; text in other scripts falls back to the system font. Run it
from the repository's root after changing a pin or the ranges:

    uv run --with fonttools==4.68.0 --with brotli==1.2.1 python3 tools/fonts/subset.py
    uv run --with fonttools==4.68.0 --with brotli==1.2.1 python3 tools/fonts/subset.py --check

Its output is committed; nothing fetches a font at build or run time.

IBM Plex's licence reserves the font name "Plex", and a subset is a modified version (OFL FAQ 2.6), so the subsets of
IBM Plex Sans and IBM Plex Mono are renamed Stage Sans and Stage Mono: in their file names and in every family, full,
unique and PostScript name. IBM's copyright notice and the licence records stay, and the licence file beside them
says what they were renamed from. Chakra Petch reserves no name and keeps its own. `--check` checks the committed
files, without fetching, and fails if a renamed font's name table names Plex outside its copyright and licence
records; every run checks what it wrote the same way.
"""

import hashlib
import sys
import urllib.request
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont

GOOGLE = "https://raw.githubusercontent.com/google/fonts/e774067705989f0bcb5e32eb0e8f76c24d679f92/ofl/chakrapetch"
PLEX = "https://raw.githubusercontent.com/IBM/plex/2e90fe366fdc27c12682db13e5aa20ded3d90908/packages"

# A renamed font's names: its family and style, the typographic family and style, and its PostScript name.
Rename = tuple[str, str, str, str, str]

# (output, source URL, the source's sha256, its new names if it is renamed)
FONTS: list[tuple[str, str, str, Rename | None]] = [
    ("ChakraPetch-Bold.woff2", f"{GOOGLE}/ChakraPetch-Bold.ttf",
     "65fbf76d95651697275e19db4d717c0e95a789ddd3476478b05292104db278a0", None),
    ("StageSans-Regular.woff2", f"{PLEX}/plex-sans/fonts/complete/ttf/IBMPlexSans-Regular.ttf",
     "b47ebd72b5ce53b60f909033ff84d6fc12358cc147e2981f8c321ad3f36246b6",
     ("Stage Sans", "Regular", "Stage Sans", "Regular", "StageSans-Regular")),
    ("StageSans-SemiBold.woff2", f"{PLEX}/plex-sans/fonts/complete/ttf/IBMPlexSans-SemiBold.ttf",
     "c51119b25f4a296da2b66edf3201b9aa1735091ed7efc1fca0313346f3851a4d",
     ("Stage Sans SemiBold", "Regular", "Stage Sans", "SemiBold", "StageSans-SemiBold")),
    ("StageMono-Regular.woff2", f"{PLEX}/plex-mono/fonts/complete/ttf/IBMPlexMono-Regular.ttf",
     "7c6fbddca4b700be918f5f6183d9bd4464fa427fe435f0b480d77fe2bb8c5a43",
     ("Stage Mono", "Regular", "Stage Mono", "Regular", "StageMono-Regular")),
]

# The name records a renamed font may still name Plex in: the copyright notice and the licence and its URL.
KEPT_AS_IS = {0, 13, 14}

# Written above IBM's licence, unchanged below it.
PLEX_CREDIT = (
    "Stage Sans and Stage Mono are IBM Plex Sans and IBM Plex Mono by IBM, subset to Latin and Latin Extended for\n"
    "the Stage Select workbench, and renamed as Stage Sans and Stage Mono because the subset is a modified version.\n"
    "IBM's licence for them follows.\n\n"
)

LICENCES = [
    ("ChakraPetch-OFL.txt", f"{GOOGLE}/OFL.txt",
     "13831d02389d917d22fcfa6c79f98f8acbf61d230add0e73a4d4dc5f3bfb9e56"),
    ("IBMPlex-OFL.txt", f"{PLEX}/plex-sans/LICENSE.txt",
     "7e6b2818edbd8f6a01ae80641cc8f16a51080d08fb4e532be3a0b6f74adb07da"),
]
CREDITS = {"IBMPlex-OFL.txt": PLEX_CREDIT}

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


def rename(font, names):
    """Renames `font` as `names` says, keeping its copyright and licence records, and drops any other record that
    names Plex."""
    family, style, typographic_family, typographic_style, postscript = names
    table = font["name"]
    unique = table.getDebugName(3) or ""
    version = unique.split(";")[0] if ";" in unique else "1.000"
    new = {
        1: family, 2: style, 3: f"{version};{postscript}", 4: f"{typographic_family} {typographic_style}"
        if typographic_style != "Regular" else typographic_family, 6: postscript, 16: typographic_family,
        17: typographic_style, 21: typographic_family, 22: typographic_style, 25: postscript.split("-")[0],
    }
    for record in list(table.names):
        if record.nameID in new:
            record.string = new[record.nameID]
        elif record.nameID not in KEPT_AS_IS and "plex" in record.toUnicode().lower():
            table.removeNames(nameID=record.nameID)
    # A CFF font also names itself in its CFF table.
    if "CFF " in font:
        font["CFF "].cff.fontNames = [postscript]


def check(path):
    """Fails unless the renamed font at `path` names Plex only in its copyright and licence records."""
    font = TTFont(str(path))
    named = [
        (r.nameID, r.toUnicode()) for r in font["name"].names
        if r.nameID not in KEPT_AS_IS and "plex" in r.toUnicode().lower()
    ]
    if "CFF " in font and any("plex" in n.lower() for n in font["CFF "].cff.fontNames):
        named.append(("CFF", font["CFF "].cff.fontNames))
    if named:
        raise SystemExit(f"{path}: still names Plex: {named}")


def main():
    if sys.argv[1:] == ["--check"]:
        for name, _, _, names in FONTS:
            if names:
                check(OUT / name)
        print("every renamed font names Plex only in its copyright and licence records")
        return
    OUT.mkdir(parents=True, exist_ok=True)
    for name, url, sha256 in LICENCES:
        (OUT / name).write_bytes(CREDITS.get(name, "").encode() + fetch(url, sha256))
    unicodes = [c for start, end in RANGES for c in range(start, end + 1)]
    total = 0
    for name, url, sha256, names in FONTS:
        source = OUT / f".{name}.ttf"
        source.write_bytes(fetch(url, sha256))
        options = subset.Options()
        options.flavor = "woff2"
        options.layout_features = ["*"]
        options.name_IDs = ["*"]
        options.name_languages = ["*"]
        options.notdef_outline = True
        options.hinting = False
        options.desubroutinize = True
        try:
            font = subset.load_font(str(source), options)
            subsetter = subset.Subsetter(options)
            subsetter.populate(unicodes=unicodes)
            subsetter.subset(font)
            if names:
                rename(font, names)
            subset.save_font(font, str(OUT / name), options)
        finally:
            source.unlink()
        if names:
            check(OUT / name)
        size = (OUT / name).stat().st_size
        total += size
        print(f"{name}: {size} bytes")
    print(f"total: {total} bytes")


if __name__ == "__main__":
    main()
