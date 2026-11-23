# Stage Select

**ROM organizer**

Identity v2.0 · 15 November 2026

> **Status.** This identity replaces the v1.1 identity, and the workbench wears it.

## Position

Stage Select puts the choice of game titles and releases at the heart of the collection. Arcade, console, handheld
and other game systems are all in scope, including MAME collections. It preserves meticulously curated Source Sets
across those systems and derives selected Play Libraries for a device, purpose or preference.

The name is the screen where a player picks what to play next: a grid of choices and a cursor on one of them. That
is also what the product does, one system and one release at a time. Write **Stage Select** in prose, two words,
both capitalized. The short descriptor is **ROM organizer**.

### Where the name comes from, and where the borrowing stops

The stage select screen is a convention of the genre, and the Mega Man series is its best-known example; the series
spans most of the systems Stage Select supports. The identity takes the convention, not the series:

- **Ours to use:** a grid of cells, one chosen; corner brackets as a cursor; a blue family; a segmented meter;
  square-cornered lettering.
- **Not ours:** any character, portrait, sprite, logo, screen layout or lettering from a published game; any
  publisher's exact palette; any platform's hardware silhouette, seal or logo.

The artwork in this kit is drawn from scratch by `make-artwork.py`. Keep it that way.

## Where the brand appears

| | Product | Website |
| --- | --- | --- |
| Name, emblem, palette, type, geometry | Yes | Yes |
| Voice | Exact: this guide's Voice section | Enthusiastic: `stage-select-website-copy.md` |
| Signature, headline, principles, "Keep. Select. Play." | **No** | Yes |
| Bands, selector rhythm, display headlines | **No** | Yes |

The product is the workbench, the CLI's help and output, and error messages. It carries no slogan and has no
landing screen: `/` goes straight to the Source Sets list. The brand shows in the product as a working tool that is
recognisably itself, not as copy.

## Emblem and lockups

The emblem is a 3 × 3 grid of square cells on a rounded plaque. The centre cell is the chosen one: Chosen yellow,
inside four Sky corner brackets. The other eight are Cobalt.

| File | Use |
| --- | --- |
| `stage-select-emblem.svg` | The emblem, 48 px and larger, on a light ground |
| `stage-select-emblem-small.svg` | 16 to 32 px: no brackets, larger cells. The favicon and the workbench's header |
| `stage-select-logo.svg` | Primary lockup: emblem beside STAGE over SELECT |
| `stage-select-logo-line.svg` | One-line lockup, where height is short |
| `…-on-dark.svg` of each | The same on a dark ground: the plaque is Deck and the lettering white |
| `stage-select-ready.svg` | The READY lettering (see Pixel lettering) |

Every file is a vector master, written by `make-artwork.py`. To change the artwork, change the script and run it;
don't edit the SVGs by hand. Export a PNG from the SVG when a raster is needed, at a whole multiple of 48 px.

- The plaque is part of the emblem. It gives the cells the same ground everywhere, so there is no reversed version
  to maintain. On a Night ground use the `-on-dark` files, whose Deck plaque stays visible.
- The emblem's colours are fixed. Don't recolour a cell to show a status.
- Keep the emblem and lettering of a lockup together, in their proportions. Leave at least one cell's width
  (a fifth of the emblem's side) clear around the lockup.
- Use the primary lockup at 160 CSS px wide or larger. Below that, use the emblem alone and write **Stage Select**
  in the interface typeface beside it.
- The lettering is artwork: strokes of one weight with square corners, leaning 10° forward, on the emblem's
  geometry. Don't retype it in a font, and don't abbreviate the name into a competing mark.
- No glow, gradient, extrusion, texture or distortion.

## Palette

### Identity colours

| Name | Hex | Role |
| --- | --- | --- |
| Night | `#0B1026` | Dark canvas; text on light; the emblem's plaque on light |
| Deck | `#141B3D` | Dark surfaces; the emblem's plaque on dark |
| Cobalt | `#2F62F0` | The emblem's cells; primary buttons in the dark theme |
| Deep cobalt | `#1847D1` | Actions, links and the cursor in the light theme |
| Light cobalt | `#8FB0FF` | Links and action text in the dark theme |
| Sky | `#3CC8F0` | The emblem's brackets; the cursor, focus and active tone in the dark theme |
| Chosen yellow | `#F8D038` | The emblem's chosen cell; the READY lettering. Nothing else |
| Frost | `#EEF2FA` | Light canvas |
| Mist | `#A4AED0` | Secondary text in the dark theme |
| Slate | `#4A5578` | Secondary text in the light theme |

Both themes sit in one blue family, so the workbench looks like the same product in light and dark.

**Chosen yellow stays out of interface states.** In the dark theme it is too close to the review tone to mean
anything else. The cursor is Sky or Deep cobalt, and it is a shape (brackets) before it is a colour.

**Red belongs to failure.** The identity has no red, so a red mark in the product always means something failed.

### Status tones

| Tone | Light | Dark | Means |
| --- | --- | --- | --- |
| Good | `#237047` | `#6FCF97` | Verified, complete, current |
| Review | `#805800` | `#E3B341` | Unresolved: needs a look or a decision |
| Failed | `#A42826` | `#FF8F85` | An actual failure or a blocked operation |
| Active | `#1847D1` | `#3CC8F0` | Running now |
| Neutral | `#4A5578` | `#A4AED0` | No finding yet |

Every status pairs its tone with a glyph and a label; no state is told apart by colour alone. Verified is green
and never blue, so it can't be mistaken for a checked row.

### Measured contrast

| Foreground / background | Light | Dark |
| --- | ---: | ---: |
| Text / canvas | 16.77:1 | 15.95:1 |
| Text / surface | 18.81:1 | 14.20:1 |
| Secondary text / canvas | 6.54:1 | 8.55:1 |
| Secondary text / checked row | 5.87:1 | 5.53:1 |
| Action text / checked row | 5.87:1 | 5.70:1 |
| Primary button label / fill | 7.34:1 | 5.09:1 |
| Lowest status tone on any list surface | 4.70:1 | 5.52:1 |
| Cursor / surface | 7.34:1 | 8.54:1 |
| Emblem cell / plaque | 3.70:1 | 3.29:1 |
| Chosen yellow / plaque | 12.60:1 | 11.22:1 |

Text and status tones meet 4.5:1 on the canvas, surfaces, hovered rows and checked rows in both themes. These
figures cover the named pairings, not the accessibility of a complete implementation.

## Typography

| Use | Family | Treatment |
| --- | --- | --- |
| Page and section titles | Chakra Petch 700 | Sentence case, upright |
| Interface | IBM Plex Sans 400, 600 | Sentence case, comfortable spacing |
| Data and technical labels | IBM Plex Mono 400 | Paths, hashes, revisions and aligned counts |

All three are under the SIL Open Font License. Font files are not in this kit; the product serves its own copies
with their licence and never loads a font from another origin. IBM Plex reserves the name "Plex", so a subset of it,
being a modified version, is renamed: the workbench serves its subsets as Stage Sans and Stage Mono.

Screen scale:

- Page title: 32/38 px.
- Section title: 20/26 px.
- Body, controls and table text: 14/20 px.
- Secondary metadata: 12/16 px.

Keep Chakra Petch to titles; it tires in a paragraph or a table. Show full file paths and hashes in details, and
make them copyable without truncation.

### Pixel lettering

There is one piece: **READY**, supplied as `stage-select-ready.svg`, drawn on a 5 × 7 grid in Chosen yellow. In
the product it may appear in one place, beside the message that a Build has finished. Scale it by whole pixels.
Don't set anything else in pixel lettering, and don't add a pixel font.

## Working interface

- Use a 4 px spacing grid, with common gaps of 8, 16 and 24 px.
- Use 2 px control corners, 1 px dividers and flat controls. No shadows on anything that isn't an overlay.
- Use 40 px rows by default, with comfortable targeting and keyboard navigation.
- Use 120–160 ms transitions and respect reduced motion.
- Lead with integrity, unresolved review items and the chosen Play Library.
- Keep collection-coverage metrics secondary to useful decisions.

### Interaction states

Each state has its own look. A row can be in several at once, and each must still show.

| State | Meaning | Look | Token |
| --- | --- | --- | --- |
| Hover | The pointer is here; nothing is chosen | A lighter neutral surface | `--color-hover` |
| Checked | In the set a toolbar action applies to | The cobalt wash, and its checkbox | `--color-checked` |
| Keyboard cursor | Where the arrow keys are | Four corner brackets | `--color-cursor` |
| Open | Shown in the detail pane | An inset edge on the leading side, bolder name | `--color-open-edge` |
| Current | The screen or tab you are on; a pressed filter | An edge along the bottom | `--color-current-edge` |
| Focus | Any other focused control | A 2 px outline, 3 px off | `--color-focus` |

The brackets are the emblem's brackets: the cursor in a list is the same shape as the cursor in the mark.

A primary button is `--color-on-action` on `--color-action-fill`. Under the pointer it moves one step, to
`--color-action-fill-hover`, and stays the same button. Links and action text use `--color-action`.

### Segmented meter

One part shows "how much of a whole": a row of equal, square-cornered segments with a 3 px gap.

- **Progress of a Job:** filled segments in the active tone, the rest outlined.
- **A Source Set's Games:** complete segments filled in the good tone, incomplete segments hatched in the review
  tone, missing segments outlined. The three differ by pattern as well as colour.

The meter describes; it never sets a goal. It sits beside the exact counts, shows no percentage for Games, and uses
no "cleared" or "complete the set" wording. A count above zero always shows at least one segment. The whole meter
is one named image whose label carries the numbers.

## Product language

`GLOSSARY.md` is the source of every product term; this kit defines none. `AGENTS.md` sets how Operator-facing
text is written (address the reader as "you", glossary terms in lowercase, no ADR citations), and
`docs/agents/ui.md` sets the workbench's copy rules.

### Claims the brand must not make

- **Verified is not playable.** It means matched to a named reference by a stated check.
- **Unmatched is not damaged.** It is an unresolved result.
- **Unselected is not unwanted.** Files outside a Selection can still have preservation value, and they stay in
  the Source Set.
- **1G1R is not one file.** One game, one selected release, and every file that release needs: discs, tracks, BIOS,
  parent and device sets, CHDs. Count selected games separately from their files.
- **A clone is not a duplicate.**
- **Bigger is not better.** No collection-size bragging and no completion pressure.

## Voice

Name the action, explain the result and make uncertainty visible. The product is exact; it doesn't cheer.

| Situation | Example |
| --- | --- |
| Empty state | Start with a source set. Choose a folder to scan and verify. |
| Review | 3 files need review. No reference match found. Inspect the details before selecting. |
| Selection | English preferred. Revision 2 selected. Other versions remain in your source set. |
| Completion | Your library is built. 620 games ready. Source files unchanged. |
| Failure | The build stopped at 412 of 620 files. Nothing was removed. Open the job to see why. |

Prefer **organizer**, **collection**, **source set**, **verify**, **select**, **build** and **play library**. Don't
call a Source Set a "stage" or a finished Audit "cleared": the name is a metaphor for the product, not a second
vocabulary inside it.
