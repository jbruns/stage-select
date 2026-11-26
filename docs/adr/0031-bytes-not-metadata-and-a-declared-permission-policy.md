# 31. Bytes on disk, not metadata, and a declared Permission Policy

Date: 2026-11-26

## Status

Accepted. Amends ADR 0023; bounds ADR 0006.

## Context

Every stamp Stage Select records (Audit stamps, claim stamps, Placed files) holds device, inode, size, modification
time and change time, and change time is deliberately load-bearing (ADRs 0008, 0010, 0023, 0024): it catches an
overwrite of the same length whose modification time was put back. But `chmod`, `chgrp`, `chown` and `setfacl` move
it too. On the Operator's Host, one recursive permission change made 23 Source Sets Stale with no byte changed, and
turned all 1486 Placed Games of a Single Copy Library Folder into permanent Conflicts: placed by rename, they had no
hash to re-verify against, and the Audit the message suggested never refreshed a Placement. Send-back, return and
unregister were all refused, a dead end the Operator escaped only by moving the files by hand. Each `.stage-select`
folder also had to be owned by Stage Select's uid with mode exactly 700, so a uid change or a group-writable tree
blocked every managed folder, with advice to move the folder away. Stage Select declared no permissions of its own,
so it could neither restore the modes the Operator intended nor say what they were.

## Decision

- **Only bytes are change.** A file whose device, inode, size and modification time hold, but whose change time, mode
  or owner moved, is re-read rather than reported. If its hashes match, its new stamp is recorded silently; only a
  difference in bytes makes a Source Set Stale or a Library Folder drift. Change-time checks inside a plan's own
  window, through held descriptors, stay as ADRs 0008, 0010 and 0024 set them.
- **A rename keeps the Audit's Evidence as its proof** (amends ADR 0023). A Placement by rename records the SHA-256
  that the Source Set's latest Audit verified for each file, and a Placed file without one is checked against its
  DAT ROM. An Audit that verifies a Placed file refreshes its Placement; one that finds a Placed Game back in the
  Source Set's own folder records it there, changing the store and not the disk.
- **Stage Select's own folders are repaired, not refused.** A `.stage-select` folder is accepted when Stage Select can
  write it and others (beyond its owner and group) have no access, whoever owns it. Stage Select corrects the modes of
  its own folders, and re-records stamps after a matching re-read, without a plan. Game files change only through an
  approved Change Plan.
- **A declared Permission Policy** (bounds ADR 0006). File mode, folder mode and an optional group, global with
  per-Root overrides; without one, the Host's defaults apply and nothing is reported. Stage Select applies it to what
  it writes, checks it from the stats it already takes, reports differences as their own finding (never Stale or
  Drift, never blocking an Audit or Build), and restores them in writable folders through an approved plan. Read-only
  Source Sets are reported only. It asks for no privilege: it changes modes only of files it owns and groups only to
  those it belongs to.
- **What it can't fix, it explains.** Where its privileges don't reach, such as a file another uid owns, Stage Select
  says what is wrong, what it expects and the exact command to run on the Host, and recovers fully on the next check.
  It refuses only an operation that truly can't proceed, with the same three parts, and never advises moving its
  folders away.

## Considered Options

- **Ignore change time altogether**: rejected, as it gives up catching a same-length overwrite with its modification
  time put back.
- **Re-create files Stage Select doesn't own** (copy, re-hash, rename over) so it can change their modes: rejected,
  as it rewrites every byte to fix one inode field that a root `chown` on the Host fixes at once.
- **Leave permissions wholly to the Host**: rejected, as Stage Select could then neither restore intended modes nor
  explain a mismatch.

## Consequences

- A mass permission change leaves the files it touched unconfirmed, not Stale. Nothing starts by itself: the next
  Incremental Audit re-reads only those files, and a Library Folder check re-reads its own, instead of a Stale status
  that only a full Audit clears.
- Stores made before this record Placed files with no hash; they are checked against their DAT ROMs until their
  Source Set's next Audit fills them in.
