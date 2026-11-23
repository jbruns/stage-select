# Stage Select brand kit

Identity v2.0 · 15 November 2026

**ROM organizer**

This kit replaces the v1.1 identity. The product ships under the new name but still in the old look; #162 tracks the
workbench's adoption of this kit.

## Files

- `stage-select-style-guide.md`: the identity, the working interface and the product's voice.
- `stage-select-website-copy.md`: the signature, headline and principles, for a future website. None of it goes
  into the product.
- `stage-select-tokens.css`: colours for both themes, status tones, interaction states, type, spacing and motion.
- `stage-select-emblem.svg`, `stage-select-emblem-small.svg`: the emblem, and its form for 32 px and below.
- `stage-select-logo.svg`, `stage-select-logo-line.svg`: the stacked and one-line lockups.
- `…-on-dark.svg` beside each of the four: the same artwork for a dark ground.
- `stage-select-ready.svg`: the READY lettering.
- `make-artwork.py`: writes every SVG above. Run `python3 docs/brand-kit/make-artwork.py` after changing it.
- `title-select-tokens.css`: the v1.1 tokens, kept only because the workbench links them until #162 lands. Don't
  build anything new on them.

## What changed from v1.1

- The emblem is a 3 × 3 grid with its centre cell chosen, drawn as vector artwork on its own plaque. It replaces
  the three-bar selector, which was generated raster artwork with no reversed version.
- Both themes share one blue family: a navy dark theme and a cool light one. The tokens file now holds both themes
  and the status tones, which the workbench's stylesheet used to declare on its own.
- Red has left the identity, so red in the product only ever means failure. Yellow is kept to the emblem's chosen
  cell and the READY lettering.
- Hover, checked, the keyboard cursor, open and current each have their own look.
- Chakra Petch, IBM Plex Sans and IBM Plex Mono replace Liberation Sans and Liberation Mono.
- Marketing copy moved to its own file and out of the product. The gradient, the campaign violet, the solid offset
  and the packaging grid are gone.
- The kit no longer keeps its own product vocabulary; `GLOSSARY.md` is the source.

Font files are not bundled. All artwork is original and drawn by the script; the style guide says what the
identity may and may not borrow from the games that inspired the name.
