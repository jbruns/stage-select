# 21. Coverage changes how Audit results are read, and filters Selection candidates

Date: 2026-11-16

## Status

Accepted

## Context

Large Redump sets are often kept as regional subsets. Every Audit counts every DAT Game, so a Source Set holding only
Europe discs reports thousands of other-region Games as Missing. Its Fixdat and Repair fills chase them too.
Selection Profiles already filter by region, but only at Selection time. Retool's own answer is to write a filtered
DAT and audit against that (#175).

## Decision

- **Coverage belongs to the Source Set.** A Source Set may declare a Coverage: the regions it intends to preserve,
  stored as an expanded list of region names. It belongs to the Source Set, not to the system or the Profile, because
  several Source Sets may share a DAT.
- **The Audit doesn't change.** Coverage is applied when its results are read. The Audit still reads every Entry and
  gives every Game its Game Status against the unmodified, pinned DAT Revision (ADR 0002). Coverage then splits
  Games into covered and not covered:
  - Summary counts, Fixdats and Repair fills consider only covered Games.
  - Games outside Coverage that are present still count and are kept.
  - Changing Coverage needs no new Audit and doesn't make the Source Set Stale.
  - Fixdats, exports and plans record the Coverage they used.
- **Selection considers only covered Games.** Games outside the Coverage are removed from Selection's candidates
  before the Selection engine runs, with a trace rule of their own. A Selection records the Coverage it used, and a
  Coverage change makes the Library Folders built from that Source Set Stale.
- **A Game is covered when any of its region tags is.** Selection's own name-tag parsing decides a Game's regions.
  World is covered by every Coverage. A Game with no region tag has the region Unknown, which no preset includes, so
  it is covered only when the Coverage names Unknown explicitly.

## Considered Options

- **A filtered DAT, as Retool does.** Rejected: an Audit would no longer pin the DAT Revision's own file, and
  "Filtered DAT" is a term the glossary avoids.
- **Applying Coverage during the Audit.** Rejected: an Audit reads every Entry anyway, and a Coverage change would
  force a new Audit just to recount.
- **Leaving Selection untouched.** Rejected: with a Europe Coverage and a Profile that prefers USA, the Selection
  would choose USA releases the Source Set never holds, leaving every Library Folder Incomplete in a predictable way.

## Consequences

- A Selection now depends on its Source Set's Coverage as well as on the DAT Revision and the Profile. Without a
  Coverage, nothing changes, including the goldens.
- Region parsing in the selection crate becomes a shared, public function, so Audit results and Selection agree.
- Combining several regional Source Sets into one Library Folder, and trimming a Source Set down to its Coverage, are
  out of scope.
