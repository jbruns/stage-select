# Redump regional splits: evidence for Coverage presets

Findings for #175. Researched 2026-11-16 from public pages only: Internet Archive search metadata (item titles, not
contents), redump.org's public disc listing for PlayStation 2, and this repo's Retool fixture
`crates/selection/tests/goldens/fixtures/Sony - PlayStation - Datfile.txt`. No DAT was downloaded and no Source Set,
Host or Operator data was read. **Inference** marks conclusions that the sources do not state directly.

## Recommendation

Four combinable Coverage presets, each expanding to engine region names (`docs/default-orders.md`):

| Preset | Regions |
| --- | --- |
| USA + Canada | USA, Canada |
| Latin America | Mexico, Brazil, Argentina, Peru, Latin America |
| Europe + Australia | Europe, UK, Ireland, Australia, New Zealand, France, Germany, Spain, Italy, Netherlands, Belgium, Austria, Switzerland, Portugal, Russia, Ukraine, Poland, Scandinavia, Sweden, Norway, Denmark, Finland, Iceland, Greece, Hungary, Czech, Slovakia, Turkey, Croatia, Slovenia, Bulgaria, Romania, Serbia, Estonia, Latvia, Lithuania, Macedonia, Albania, Israel, South Africa |
| Asia | Japan, Asia, Korea, Taiwan, Hong Kong, China, Thailand, Singapore, Indonesia, India, United Arab Emirates |

World needs no preset: it is covered by every Coverage. The evidence is thin, so the presets are a starting point,
and the Operator can edit the expanded list.

## 1. How the large Redump sets are split in public collections

- Most public Redump collections are **not split by region**. They are split by title initial or into numbered parts.
  For example, PlayStation 2 items `redump_ps2` and `SonyPS2RedumpISOSet_2020-01_01`, and PlayStation item
  `ef_Sony_PlayStation1_Redump_Collection_2of4`, are split this way.
- The only confirmed regional splits are PlayStation 2 items on the Internet Archive:
  - **Japan**: "Redump - Sony Playstation 2 - Japan - Part N" (`ps2japanredump1` to `ps2japanredump5`).
  - **Japan and Asia, bundled**: "[REDUMP] Sony - PlayStation 2 (Japan, Asia) Part 2"
    (`redump-sony-playstation-2-japan-asia-part-2`).
  - **USA**: "Redump - Sony - PlayStation 2 (2018-Aug-01) (USA) …" (`ps2usaredump1_20200816_1458`,
    `redumpSonyPlaystation2UsaGames2018Aug01Part4`).
- No Europe-only item surfaced. No region split was confirmed for PlayStation, GameCube or Wii.
- **Inference:** the regional items bucket by macro-region, and Asia travels with Japan. How they place World and
  multi-region discs is unknown.

## 2. Redump's own region groups

- redump.org's disc listing (`http://redump.org/discs/system/ps2/`) filters by World, Europe, America, Asia and
  "Twins".
- On the first page of the Asia filter, rows carried the country icons Japan (86), Korea (13) and Asia (1). So
  Redump's "Asia" is a macro-region that includes Japan and Korea.
- Which countries fall under Europe and America was not established.
- The region names seen (Japan, Spain, Korea, Asia) match the engine's names.
- Per-region counts were not obtainable, because the filter does not change the pager. Unfiltered totals: PlayStation 2
  11,774 discs, and from the pager, about 10,900 PlayStation, 3,700 Wii and 2,000 GameCube.

## 3. Region tags in the PlayStation fixture

- **Method:** the first parenthesised group containing a known region name, counted per line, over 15,703 lines.
- **Caveat:** the fixture is a frozen Retool test input, not a current DAT.
- **Counts:**
  - Japan 6,351; Europe 3,233; USA 3,166.
  - Germany 693; France 638; Spain 419; Italy 365.
  - Japan, Asia 278; Europe, Australia 146.
  - Everything else, including Korea 14, World 2 and USA, Europe 2, is below 60.
- **Single European countries are about a quarter of the lines.** A Europe Coverage that names only "Europe" would
  miss them, so the Europe + Australia preset lists the countries.
- **"Japan, Asia" and "Europe, Australia" are common pairings.** This supports bundling Japan with Asia and Australia
  with Europe.

## 4. Edge cases

- **Multi-region names**, such as `(USA, Europe)` or `(Japan, Asia)`: covered when any of their regions is. This is
  the design's rule, not one taken from any split source.
- **Latin America and Mexico:** neither appears in the fixture. Mexican releases are usually Latin American editions,
  so Mexico sits with Latin America, not with USA + Canada (**inference**).
- **Turkey, Israel and South Africa** are in Europe + Australia, as PAL markets. **India and United Arab Emirates**
  are in Asia. All of this is **inference**, unconfirmed by any split.
- **GameCube and Wii** are assumed to follow the same tagging as PlayStation and PlayStation 2. This is not verified.

## Open

- Confirm the Europe and America filter membership on redump.org, and whether any Europe-only or PlayStation, GameCube
  or Wii regional split exists, before treating the presets as settled.
