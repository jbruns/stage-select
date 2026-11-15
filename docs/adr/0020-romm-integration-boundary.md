# 20. RomM integration: scan requests and one collection, never identity

Date: 2026-11-14

## Status

Accepted

## Context

Research 0005 kept artwork and metadata in RomM, which reads the Play Library mounted `:ro`. The Operator wants
tighter coordination: prompt rescans after a Build, and RomM's identification agreeing with the DAT. RomM identifies a
game by provider ids (IGDB, ScreenScraper and others) that Title Select doesn't know. Its `verified` flag is set only by
Hasheous or RetroAchievements, and it imports no DATs (research 0136).

## Decision

- **Optional.** RomM is an optional integration, off by default. Title Select works fully without it, and a RomM
  failure is a warning on a Job, never a failure.
- **What Title Select sends.** It requests platform-scoped scans after a Build or cleanup that changed a Library
  Folder, and maintains one "Additions" collection. It never writes provider ids, names or other identity into RomM.
- **Identity.** Title Select's authority reaches RomM through DAT names and payload hashes in the files it builds.
  RomM is deployed with Hasheous off, ScreenScraper and libretro supplying metadata. Title Select reads RomM back only
  to show disagreements, and offers per-game rescans for them.
- **Credentials.** A RomM Client API Token belonging to a dedicated non-admin user is entered in the workbench and kept
  in the store. It is write-only through the API and plain text at rest, like the API keys that \*arr-style
  applications keep.

## Considered Options

- **Impersonating Hasheous**: pointing RomM's `HASHEOUS_API_URL` at Title Select so our Audits set RomM's `verified`
  flag. Rejected: it binds us to an undocumented API that can change in any RomM release, and it still supplies no
  provider ids.
- **One collection per Selection Profile.** Rejected: RomM sees only Library Folders, so such a collection would repeat
  the whole library.
- **Token in the config file.** Rejected in favour of the workbench, where the other integration settings live. An
  encryption key stored beside the store would protect little.
