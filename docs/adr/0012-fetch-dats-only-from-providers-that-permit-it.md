# 12. Fetch DATs only from DAT Providers that permit it; never contact No-Intro

Date: 2026-11-03

## Status

Accepted

## Context

Following (#91) marks a Source Set Stale when a newer DAT Revision has been imported, but only manual import exists,
and the runtime has no HTTP client. Following needs newer revisions to arrive with less effort, but DAT Providers
differ:

- **No-Intro** publishes no sanctioned automated access. DAT-o-MATIC's download is a session-based two-step form,
  and automated requests with incorrect parameters have already led to a ban. igir and other tools document manual
  download only; oxyromon polls a version file but refuses to download; RomVault's DatVault is a separate paid proxy.
- **Redump** serves each system's DAT, cue pack and SBI pack at stable unauthenticated URLs (`/datfile/<code>/`,
  `/cues/<code>/`, `/sbi/<code>/`). Open-source tools fetch them with a courtesy throttle. No published terms or
  rate limit were found.
- **MAME** publishes ListXML as release assets on GitHub, and its software lists are CC0 in the source tree.

## Decision

- Stage Select never sends any request to No-Intro, not even a version check. No-Intro DATs arrive only as files the
  Operator drops in, and Following recognizes a newer revision from the DAT itself.
- Fetching is opt-in per DAT Provider, off by default, and limited to Following Source Sets' systems. Redump: at
  most one DAT, one cue pack and one SBI pack per system per day, a descriptive User-Agent, at least a second between
  requests, and stop at the first error. MAME: new releases found through the GitHub Releases API.
- No silent fallback to an unofficial mirror. A failed fetch leaves the last good revision in place and is shown.

## Consequences

- No-Intro Following stays partly manual. Revisit only if No-Intro publishes sanctioned automated access.
- Fetched DATs become DAT Revisions like imported ones; fetching never starts an Audit (Follow is unchanged).
- Redump cue packs become a Repair source for `.cue` ROMs; SBI files never enter a Source Set (ADR 0013).

## Amendment (2026-11-20): MAME support files

The arcade selector (ADR 0027) reads progetto-SNAPS's `catver.ini`, `languages.ini` and `bestgames.ini`. They carry
no licence, so they are never shipped in the image; Stage Select fetches them from their maintainer's GitHub mirror
instead, and credits the maintainer.

- Under the same `[fetch] mame` switch as MAME's DATs, off by default: no request without the Operator's consent.
- From the mirror's commit resolved through the GitHub API, when a MAME Source Set first gets a ListXML DAT Revision
  and whenever a newer one arrives; at most once a day, with ADR 0012's courtesy rules.
- Each file is kept like a DAT Revision, by its SHA-256, with the MAME version its header names. A Selection records
  the revisions it used; a version behind the ListXML is shown as drift, never a failure.
- The Operator may drop the files in instead, as with No-Intro DATs.
- A failed fetch leaves the last good files in place and is shown.
