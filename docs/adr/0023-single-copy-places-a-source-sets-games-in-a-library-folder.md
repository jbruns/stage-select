# 23. Single Copy places a Source Set's Games in a Library Folder

Date: 2026-11-18

## Status

Accepted. Amends ADR 0002, ADR 0010 and ADR 0013.

## Context

A Build copies each selected Game from its Source Set into a Library Folder, so every selected Game is kept twice.
Block cloning (ADR 0006) makes a copy nearly free only when both files share a format and a filesystem that clones.
It does not help when they differ: a TorrentZip Source Set built loose, a disc Source Set built to CHD, a repack, or
a Host whose filesystem does not clone. Organizers such as Sonarr keep one copy and move it into the library. Title
Select is a preservation tool first, so it can't adopt that by default, but some Operators choose space over a
second copy, and that choice is theirs to make.

## Decision

- **Opt-in, in the Storage Policy.** Single Copy is a Storage Policy setting, global with per-system overrides and off
  by default. Under it a Build places each selected Game in one Library Folder instead of copying it.
- **A Placed Game stays its Source Set's.** A Game has one Placement: the Source Set's own folder or a Library Folder.
  Audits, Coverage, Game Status and Fixdats read the Source Set across both, so a Placed Game is never Missing. Its
  Audit is the authority on it: a changed or vanished Placed Game is an Audit Finding or a Missing Game, and Drift
  checks still report it as an early warning. A Repair changes a Placed Game where it is placed. A Repair fill still
  writes into the Source Set's folder, and the next Build places it.
- **One holding Library Folder.** Each Source Set names the Library Folder that holds its Placed Games, by default the
  only one bound to it. Other Library Folders bound to it are built by copying, from wherever each Game is placed.
- **A Build may change its Source Set.** A Build plan's Placement items are marked as changing the Source Set, and
  approving the plan approves them. While it is queued or running, the Source Set is locked as for a Repair. This
  amends ADR 0002 and the separation of Repair and Build approval.
- **Moving.** Where the Source Set and the Library Folder share a mount, a Placement is a rename that never
  replaces. It is checked by identity, its new change time recorded through a held descriptor as ADR 0010 does, and
  not re-hashed: the payload Evidence in hand still stands. Otherwise it is a `copy_file_range` copy, re-hashed, and
  the original is deleted under ADR 0010's trusted-stamp rule, or kept when that can't be proven. Under Single Copy
  this deletion needs no Delete Originals After Conversion: opting in is the consent.
- **Returning home.** A Placed Game that a Build no longer selects, or every Placed Game once Single Copy is turned off,
  is returned to the Source Set's folder by the next plan, never set aside as a Kept File. A Library Folder holding
  Placed Games, or their Source Set, can't be unregistered until they are returned. A return that won't fit in the
  Source Set's Root is refused before the plan runs.
- **Converted sole copies are a further choice, per system.** Placing a Game in a converted format, such as CHD,
  reopens ADR 0013 for the Operator who chooses it, with its trade-offs explained. A CHD is placed only after
  extracting it gave back every track exactly, as ADR 0013 already requires, and the DAT-exact `.cue` is kept in the
  store as a content-addressed blob, since a CHD does not hold it. Returning it home extracts the tracks, writes the
  kept `.cue`, and verifies both against the DAT; one that doesn't verify stays placed and is flagged. A Full Audit
  decompresses a placed CHD and gives Payload Evidence; an Incremental Audit reuses that while the stamp is unchanged;
  a Quick Inventory takes the CHD's header SHA-1 as Claim Evidence.
- **Additions Sets** are not placed.

## Considered Options

- **The Library Folder takes over the Game**, so the Source Set shrinks: rejected, as it splits preservation across
  two concepts and makes a Placed Game look Missing.
- **Two linked plans**, a Repair releasing the Games and then a Build: rejected as ceremony, since opting in is the
  standing consent.
- **Kept Files for moved originals**: rejected, as they would quietly double the space Single Copy exists to save.

## Consequences

- With Single Copy on, a Game deleted by hand from a Library Folder is lost unless the Operator's own backups hold it;
  the workbench marks Placed Games as the only copy.
- Renames need the Source Set and the Library Folder on one mount, so a Host that binds each folder separately gets
  copies instead. The plan says which Games rename and which copy.
- A Full Audit of a Source Set with placed CHDs costs a decompression of each.
