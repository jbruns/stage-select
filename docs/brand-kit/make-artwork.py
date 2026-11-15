#!/usr/bin/env python3
"""Writes the Stage Select artwork beside this file: the emblem, the lockups and the READY lettering.

The artwork is drawn here, from the numbers below, so every file is a vector master and a change is one edit and
one run: `python3 docs/brand-kit/make-artwork.py`. It needs only the standard library.
"""

from pathlib import Path

HERE = Path(__file__).parent

NIGHT = "#0B1026"
DECK = "#141B3D"
COBALT = "#2F62F0"
SKY = "#3CC8F0"
YELLOW = "#F8D038"
WHITE = "#FFFFFF"

# The emblem's box. Every lockup is laid out in the same units.
BOX = 48


def emblem(plaque: str, brackets: bool) -> str:
    """The 3 x 3 grid with its centre cell chosen. Without brackets the cells grow, for 32 px and below."""
    cell, starts = (8.5, (7, 19.75, 32.5)) if brackets else (10, (6, 19, 32))
    out = [f'<rect width="{BOX}" height="{BOX}" rx="5" fill="{plaque}"/>']
    for row, y in enumerate(starts):
        for col, x in enumerate(starts):
            fill = YELLOW if (row, col) == (1, 1) else COBALT
            out.append(f'<rect x="{x}" y="{y}" width="{cell}" height="{cell}" fill="{fill}"/>')
    if brackets:
        out.append(
            '<path d="M17.6 21.6V17.6H21.6M26.4 17.6H30.4V21.6M30.4 26.4V30.4H26.4M21.6 30.4H17.6V26.4" '
            f'fill="none" stroke="{SKY}" stroke-width="1.5"/>'
        )
    return "".join(out)


# The wordmark's letters: strokes of one weight on a box 7 high and `w` wide, square at every corner, so the
# lettering shares the emblem's geometry. Each letter is a list of polylines.
CAP = 7.0
WEIGHT = 1.45
H = WEIGHT / 2


def letter(ch: str, w: float) -> list[list[tuple[float, float]]]:
    top, mid, low, right = H, CAP / 2, CAP - H, w - H
    return {
        "S": [[(w, top), (H, top), (H, mid), (right, mid), (right, low), (0, low)]],
        "T": [[(0, top), (w, top)], [(w / 2, top), (w / 2, CAP)]],
        "A": [[(H, CAP), (H, top), (right, top), (right, CAP)], [(H, 4.4), (right, 4.4)]],
        "G": [[(w, top), (H, top), (H, low), (right, low), (right, 3.9), (w * 0.48, 3.9)]],
        "E": [[(w, top), (H, top), (H, low), (w, low)], [(H, mid), (w - 0.9, mid)]],
        "L": [[(H, 0), (H, low), (w, low)]],
        "C": [[(w, top), (H, top), (H, low), (w, low)]],
    }[ch]


GAP = 1.5
LEAN = 0.176  # tan(10 degrees): the lettering leans forward


def word(text: str, w: float) -> tuple[str, float]:
    """The path data of `text` set with letters `w` wide, and its width."""
    d, x = [], 0.0
    for ch in text:
        for line in letter(ch, w):
            d.append("M" + "L".join(f"{round(x + px, 3)} {round(py, 3)}" for px, py in line))
        x += w + GAP
    return "".join(d), x - GAP


def lettering(text: str, w: float, x: float, y: float, scale: float, colour: str) -> str:
    d, _ = word(text, w)
    # Lean about the baseline, so the foot of each letter stays where the layout put it.
    matrix = f"matrix({scale} 0 {-LEAN * scale} {scale} {x + LEAN * CAP * scale} {y})"
    return (
        f'<path transform="{matrix}" d="{d}" fill="none" stroke="{colour}" '
        f'stroke-width="{WEIGHT}" stroke-linejoin="miter" stroke-miterlimit="4"/>'
    )


def svg(width: float, height: float, title: str, body: str) -> str:
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {round(width, 2)} {height}" role="img" '
        f'aria-label="{title}"><title>{title}</title>{body}</svg>\n'
    )


SPACE = 14  # between the emblem and the lettering


def stacked(ink: str, plaque: str) -> str:
    """STAGE over SELECT, both lines the same width, beside the emblem."""
    scale, between = 2.5, 5.0
    _, width = word("SELECT", 5)
    stage_w = (width - 4 * GAP) / 5
    top = (BOX - (2 * CAP * scale + between)) / 2
    x = BOX + SPACE
    body = (
        emblem(plaque, True)
        + lettering("STAGE", stage_w, x, top, scale, ink)
        + lettering("SELECT", 5, x, top + CAP * scale + between, scale, ink)
    )
    return svg(x + width * scale + LEAN * CAP * scale, BOX, "Stage Select", body)


def line(ink: str, plaque: str) -> str:
    """STAGE SELECT on one line, beside the emblem."""
    scale = 3.0
    _, stage = word("STAGE", 5)
    _, select = word("SELECT", 5)
    y = (BOX - CAP * scale) / 2
    x = BOX + SPACE
    x2 = x + (stage + 4.5) * scale
    body = (
        emblem(plaque, True)
        + lettering("STAGE", 5, x, y, scale, ink)
        + lettering("SELECT", 5, x2, y, scale, ink)
    )
    return svg(x2 + select * scale + LEAN * CAP * scale, BOX, "Stage Select", body)


READY = {
    "R": ["11110", "10001", "10001", "11110", "10100", "10010", "10001"],
    "E": ["11111", "10000", "10000", "11110", "10000", "10000", "11111"],
    "A": ["01110", "10001", "10001", "11111", "10001", "10001", "10001"],
    "D": ["11110", "10001", "10001", "10001", "10001", "10001", "11110"],
    "Y": ["10001", "10001", "01010", "00100", "00100", "00100", "00100"],
}


def ready() -> str:
    """READY on a 5 x 7 grid of whole cells: the one piece of pixel lettering."""
    cells = []
    for i, ch in enumerate("READY"):
        for y, row in enumerate(READY[ch]):
            for x, on in enumerate(row):
                if on == "1":
                    cells.append(f'<rect x="{i * 6 + x}" y="{y}" width="1" height="1"/>')
    return svg(29, 7, "Ready", f'<g fill="{YELLOW}" shape-rendering="crispEdges">{"".join(cells)}</g>')


FILES = {
    "stage-select-emblem.svg": svg(BOX, BOX, "Stage Select", emblem(NIGHT, True)),
    "stage-select-emblem-on-dark.svg": svg(BOX, BOX, "Stage Select", emblem(DECK, True)),
    "stage-select-emblem-small.svg": svg(BOX, BOX, "Stage Select", emblem(NIGHT, False)),
    "stage-select-emblem-small-on-dark.svg": svg(BOX, BOX, "Stage Select", emblem(DECK, False)),
    "stage-select-logo.svg": stacked(NIGHT, NIGHT),
    "stage-select-logo-on-dark.svg": stacked(WHITE, DECK),
    "stage-select-logo-line.svg": line(NIGHT, NIGHT),
    "stage-select-logo-line-on-dark.svg": line(WHITE, DECK),
    "stage-select-ready.svg": ready(),
}

if __name__ == "__main__":
    for name, text in FILES.items():
        (HERE / name).write_text(text)
        print(name)
