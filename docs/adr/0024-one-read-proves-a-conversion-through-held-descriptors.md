# 24. One read proves a Conversion, through descriptors held to the end

Date: 2026-11-19

## Status

Accepted. Amends ADR 0010 and ADR 0023.

## Context

ADR 0010 proves an original unchanged before deleting it by hashing it again at each step: a Build Conversion read
each original whole four times and each converted file three times, and a Repair Conversion read its staged ZIP twice
(#196). A change time moves on any write and cannot be set from user space, so a descriptor held from the one read
through every rename proves what the later hashes did, except across the instant between a rename and the `fstat`
that records its new change time. ADR 0023 already accepts that instant for a Placement by rename.

## Decision

- **One read of each original.** The conversion record's read holds its descriptor open through the keep-aside
  rename, the rename into `.deleting` and the unlink. Before each rename the descriptor's change time must equal the
  last one recorded, and after it the new change time is read through the same descriptor and recorded. Nothing is
  hashed again while that descriptor is held.
- **One read of each converted file.** Verifying the staged file against the ROMs and stamping its SHA-256 are one
  pass. Before the original is deleted, the converted file is proven by the staging descriptor held through the
  publish: the published path has its identity, and its change time is the one recorded after the publish.
- **One deletion path.** Every deletion under the trusted-stamp rule (Build and Repair Conversions, Placements copied
  across mounts, Additions) uses it.
- **A stop still re-reads.** A stop loses every held descriptor, so recovery, and a plan disposing of an original a
  stop left behind, still hash through a fresh descriptor as ADR 0010 requires. A stamp from a path still never
  authorizes a deletion.

## Consequences

- A write landing between a rename and the `fstat` after it is taken for the rename's own change: that window is
  as small as one system call and is the only one this decision opens.
- A Conversion holds one descriptor per original until it is disposed of; ADR 0010 already disposes of each before
  the next is written.
