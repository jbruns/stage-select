# 26. MAME Source Sets hold non-merged ZIPs and merged Disks; Play Libraries hold self-contained Games

Date: 2026-11-20

## Status

Accepted. Amends ADR 0013 and ADR 0023 for MAME systems.

## Context

MAME sets come in three layouts: merged (a family in its parent's ZIP), split (a clone's ZIP holds only what differs
from its parent) and non-merged ("absolutely everything necessary for a given game to run in one ZIP file", in MAME's
own documentation). The main public catalogue offers all three for ROMs, and its non-merged set carries each machine's
BIOS and device ROMs inside its ZIP; it offers CHDs merged only, `<parent>/<disk>.chd` (research 0235). MAME's
ListXML lists a machine's BIOS ROMs with `merge=` but its devices' ROMs only under the devices.

ADR 0013 keeps CHDs out of Source Sets because Redump and No-Intro DATs hash the tracks. A MAME DAT lists each disk by
the CHD's own SHA-1, so there the CHD is the DAT-exact file. Title Select's aims are a well-kept Source Set and a Play
Library that is ready to play.

## Decision

- **Set Layout is a Storage Policy setting of MAME systems**: non-merged, split or merged, global with per-system
  overrides. An Audit accepts any of them, counting a family's files as ADR 0025 says; a Repair proposes repacking
  towards the chosen one, each item on by default, and an Entry the layout keeps elsewhere becomes a Kept File only
  once a copy exists where the layout wants it.
- **Non-merged is the default.** A non-merged ZIP is expected to hold the Game's own ROMs and every ROM of its
  Dependencies, derived from the DAT Revision through `romof` and `device_ref`; such Entries are Matched, not
  Misnamed or Unmatched. It costs the most space, but it is what Operators collect for play, a damaged shared ROM has
  fill sources in every sibling, and it makes a Build a plain copy that can be block-cloned or placed. An Operator who
  prefers space chooses merged or split, and pays with a repack at every Build.
- **Disks are always merged.** A family's Disks sit loose in a folder named for its parent, `<parent>/<disk>.chd`,
  beside the ZIPs and whatever the ROM layout: that is the only layout CHD sets are distributed in, and no family has
  two different disks of one name. This is the one exception to a release sharing one format. A software list's Disks
  sit at `<list>/<software>/<disk>.chd`.
- **Disks are DAT-exact files**, so a MAME Source Set may hold CHDs, as ADR 0013's amendment allows for any DAT that
  lists compressed images. #228's Evidence rules apply: header SHA-1 as Claim Evidence; `chdman verify`, requiring its
  `Overall SHA1 verification successful!` line and never `-f`, as Payload Evidence; only self-contained CHD v5. A delta
  CHD is Unreadable, needing its parent; an older CHD is Unmatched, to be updated outside Title Select.
- **Wording.** User-facing text calls the layouts non-merged, split and merged, and a disk set "merged CHD set"; a CHD
  that needs its parent is a "delta CHD", never "merged".
- **A MAME Library Folder holds self-contained Games, flat.** Each selected Game is `<shortname>.zip`, non-merged with
  its Dependencies' ROMs inside, and its Disks are copied into `<shortname>/<disk>.chd`; no BIOS or device ZIP is built.
  This is what MAME, RetroArch's current `mame` core and ES-DE's `arcade` and `mame` systems read. ADR 0019's disc
  layout doesn't apply. The Library Folder shows the MAME version its Selection was built for, and that only an
  emulator of that version runs it: the Operator chooses which MAME to preserve and deploy by what the Source Set
  Follows.
- **RomM shows a Disk folder as a separate game.** Its scanner takes every folder as a multi-file game. Title Select
  doesn't write RomM's configuration (ADR 0020), so a MAME Library Folder's page lists its Disk folders for the
  Operator to add to RomM's `exclude.roms.multi_file.names`, and says what happens without them.
- **Single Copy** is offered for a MAME system only when its Source Sets are non-merged, where a Winner's ZIP is the
  Source Set's own. Disks are always copied, never placed: a merged Disk folder may serve several Games.
- **Ingest.** A folder named for a Game of a registered MAME Source Set's DAT Revision and holding only `.chd` files is
  assessed as that family's Disks, each verified before any plan moves it; any other folder stays Unsupported.

## Considered Options

- **Merged as the default**, matching MAME's design and the CHD sets: rejected as the default because every Build
  becomes a repack and Single Copy becomes impossible.
- **Non-merged without Dependencies, plus separate BIOS and device ZIPs** in Play Libraries: rejected, as front ends
  then list or must hide those ZIPs, and one Game can't be copied alone.
- **Grouping each Game's ZIP and Disks in one folder for RomM**: rejected, as MAME then can't find the Disks.

## Consequences

- A non-merged Source Set repeats every shared ROM; its Full Audit reads each copy.
- A Full Audit of Disks decompresses each, at about 170 MiB/s on one core; Incremental Audits reuse it.
- Older emulators, such as MAME 2003-Plus or MAME 2010, need sets Title Select doesn't build.
