# 19. One ES-DE disc layout, which RomM also reads

Date: 2026-11-14

## Status

Accepted. Supersedes ADR 0014's layout of Multi-disc Games in `.multidisc/`.

## Context

ADR 0014 put every file of a Multi-disc Game flat in `<system>/.multidisc/`, with an `.m3u` in the system folder. RomM
5.4.1 skips dot-folders inside a platform folder, so it never indexes those discs. It treats the `.m3u` as a
standalone ROM that downloads as the playlist alone (research 0136). ES-DE treats a folder whose name ends in an
extension as that file, and RomM treats any non-dot folder as one multi-file game.

## Decision

The ES-DE layout puts a Multi-disc Game in `<system>/<release>.m3u/`. The folder holds its generated `<release>.m3u`,
which lists each disc's entry file by bare name in disc order, and every file of every disc beside it. Single-disc
rules are unchanged. The `.multidisc/` layout is removed rather than kept as a choice, since the new layout serves
ES-DE and RomM alike. The flat layout stays. There is no migration: the Operator starts over.

## Consequences

- A `<release>.m3u/` folder follows the rules ADR 0014 gave `<game>.cue/` folders: it is covered by Drift checks, its
  removals become Kept Files, and a Build removes it once emptied only if a Build made it.
- The rest of ADR 0014 stands: a release is selected whole, and it is one Build unit.
