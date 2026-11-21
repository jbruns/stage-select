# 25. A MAME Game counts its family's files and names its Dependencies

Date: 2026-11-20

## Status

Accepted. Amends ADR 0017.

## Context

A MAME machine needs more than its own ROMs to run. Its ListXML entry lists the ROMs it shares with its parent and
its BIOS, each marked `merge=`, and names its device sets through `device_ref`; the devices' ROMs are listed only under
the devices themselves. ADR 0017 counts only a Game's own container, plus its `cloneof` parent's, so every Neo Geo game
in a split or merged set would read Incomplete for want of BIOS ROMs that sit in `neogeo.zip`, and nothing would say
that a machine can't run without `namco51`. Research 0235 also found disks shared without `merge=`: 16 non-clones share
one with their BIOS, and 26 software-list clones share one with their parent by SHA-1 alone.

MAME DATs also carry ROMs marked `status="nodump"` (no checksums), `status="baddump"` (checksums of a known bad dump)
and `optional="yes"`, and entries that list no ROM at all, such as the device `z80`. Today a nodump ROM matches any
file of its size (#232), and a Game with no ROMs audits Complete with nothing on disk (#231).

## Decision

- **A Game's ancestors' containers are its own.** For counting, a Game's files are those in its own container and in
  each ancestor's: its `cloneof` parent and its `romof` chain up to the BIOS. A ROM counts when an Entry there carries
  its name, or, failing that, its hashes under another name, as MAME's own search does. An Entry only in an unrelated
  Game's container is still Available Elsewhere. This generalises ADR 0017's parent rule.
- **Dependencies are Games of their own.** A BIOS or device Game that a Game reaches through `romof` or `device_ref`,
  transitively, is its Dependency. A Dependency has its own Game Status and never changes the Game's: a machine whose
  own files are all present is Complete, and its Audit says separately whether each Dependency is Complete. Folding
  Dependencies into Game Status, as MAME's audit does, was rejected: one missing device would make hundreds of
  machines Incomplete while nothing about their own files was wrong.
- **A Selection carries Dependencies in a role of their own.** Beside Winner, Alternative and Exclusion, a Selection
  lists each Dependency of its Winners once, with the Winners that need it as its rule. A Dependency is never elected
  and can't be excluded. A Library Folder whose Build lacks a Dependency of a selected Game is Incomplete, with that as
  the reason. The workbench shows Dependencies collapsed to one summary line per Game or Selection, expanded on demand.
- **nodump never matches.** A ROM with no good dump known matches no file and is left out of its Game's Game Status;
  the Game shows it as not dumped. An Entry under its name is Mismatched, with that reason, and a Repair keeps it by
  default rather than setting it aside, since MAME loads it.
- **baddump matches its published hashes.** It counts, can be Verified, and the Game shows that its dump is known bad.
- **optional need not be present.** An optional ROM counts when present; its absence doesn't keep a Game from
  Complete, and is shown.
- **Nothing to Check is a fourth Game Status.** A Game that lists no ROM that must be present and can match, such as a
  ROM-less device or a machine of nodump ROMs, is Nothing to Check: it is left out of the Complete, Incomplete and
  Missing counts and out of Fixdats, and satisfies any Game it is a Dependency of.

## Consequences

- Game Status stays a verdict on a Game's own files, so the Operator sees what's wrong where it is; playability is the
  Dependencies line and, for a Play Library, Incomplete.
- These rules hold for every DAT, not only MAME's: a Logiqx DAT with `romof`, nodump or optional ROMs counts the same
  way.
- Disks are ROMs for all of this (ADR 0026).
