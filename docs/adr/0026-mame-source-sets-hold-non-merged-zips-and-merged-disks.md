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
the CHD's own SHA-1, so there the CHD is the DAT-exact file. Stage Select's aims are a well-kept Source Set and a Play
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
  two different disks of one name. This is the one exception to a release sharing one format. A Disk a Game shares
  with its BIOS (`romof`, `merge=`) sits in the BIOS's folder under the BIOS's name for it, as a merged set keeps it
  there: the furthest ancestor that lists the Disk holds it, and ADR 0025 counts it from there. A software list's
  Disks sit at `<list>/<software>/<disk>.chd`.
- **Disks are DAT-exact files**, so a MAME Source Set may hold CHDs, as ADR 0013's amendment allows for any DAT that
  lists compressed images. #228's Evidence rules apply: header SHA-1 as Claim Evidence; `chdman verify`, requiring its
  `Overall SHA1 verification successful!` line and never `-f`, as Payload Evidence; only self-contained CHD v5. A delta
  CHD is Unreadable, needing its parent; an older CHD is Unmatched, to be updated outside Stage Select.
- **Wording.** User-facing text calls the layouts non-merged, split and merged, and a disk set "merged CHD set"; a CHD
  that needs its parent is a "delta CHD", never "merged".
- **A MAME Library Folder holds self-contained Games, flat.** Each selected Game is `<shortname>.zip`, non-merged with
  its Dependencies' ROMs inside, and its Disks are copied into `<shortname>/<disk>.chd`; no BIOS or device ZIP is built.
  This is what MAME, RetroArch's current `mame` core and ES-DE's `arcade` and `mame` systems read. ADR 0019's disc
  layout doesn't apply. The Library Folder shows the MAME version its Selection was built for, and that only an
  emulator of that version runs it: the Operator chooses which MAME to preserve and deploy by what the Source Set
  Follows.
- **RomM shows a Disk folder as a separate game.** Its scanner takes every folder as a multi-file game. Stage Select
  doesn't write RomM's configuration (ADR 0020), so a MAME Library Folder's page lists its Disk folders for the
  Operator to add to RomM's `exclude.roms.multi_file.names`, and says what happens without them.
- **Single Copy** is offered for a MAME system only when its Source Sets are non-merged, where a Winner's ZIP is the
  Source Set's own. Disks are always copied, never placed: a merged Disk folder may serve several Games.
- **Ingest.** A folder named for a Game of a registered MAME Source Set's DAT Revision and holding only `.chd` files,
  hidden files and partial downloads aside, is assessed as that family's Disks, each verified before any plan moves it;
  any other folder stays Unsupported.

## Considered Options

- **Merged as the default**, matching MAME's design and the CHD sets: rejected as the default because every Build
  becomes a repack and Single Copy becomes impossible.
- **Non-merged without Dependencies, plus separate BIOS and device ZIPs** in Play Libraries: rejected, as front ends
  then list or must hide those ZIPs, and one Game can't be copied alone.
- **Grouping each Game's ZIP and Disks in one folder for RomM**: rejected, as MAME then can't find the Disks.

## Consequences

- A non-merged Source Set repeats every shared ROM; its Full Audit reads each copy.
- A Full Audit of Disks decompresses each, at about 170 MiB/s on one core; Incremental Audits reuse it.
- Older emulators, such as MAME 2003-Plus or MAME 2010, need sets Stage Select doesn't build.

## Amendment (2026-11-20): ListXML Disks only

Software lists are out of M6 (ADR 0027's amendment), so Disks come only from ListXML machines. The software-list Disk
layout above, `<list>/<software>/<disk>.chd`, is recorded for a later milestone and not built now.

## Amendment (2026-11-21): a repack sets no spare copy aside by default

A Repair towards a Set Layout only repacks by default: its Builds, Renames, Fills and conversions run, and each keeps
aside only the originals it replaces, once what it writes is verified. A file left holding only copies of ROMs that
another file holds, or that the Repair writes, is left where it is by default, and the item says so; the Operator may
choose to keep it aside. A file holding only Unmatched Entries is still kept aside by default, as it holds no ROM.

Review found, round after round, a way for a default plan to set aside the last usable copy of a ROM, because what
made a file spare was a write another item makes, and that write can fail after approval: a Fill whose Ingest Folder
item is gone, say. So a Keep, chosen or not, needs every earlier step of the plan, in any item, that reads the file or
writes a payload it holds, and the Publish of each such Build where it comes first: if one of them fails, the Keep is
skipped and the file stays. A Keep that comes before the only write of another item covering what it holds, such as a
file in the way of a new one before a later Fill, can't wait for it, so the plan leaves its item. A conversion's writes
come after, but each reads only its own originals, which stay until it is verified. An item's own Fill still comes
after the Keep of a file in its way, as it did before this amendment.

## Amendment (2026-11-25): a duplicate whole files already hold is set aside by default

A file holding only copies of ROMs is kept aside by default when every ROM it holds is in a file that is there now and
stays as it is, a Game's own file the plan leaves whole, and no applied item reads it: a duplicate of a Game under an
older name, say. What makes it spare is no write of this Repair, which could fail, so the 2026-11-21 amendment's reason
for leaving it doesn't apply. A file whose copies depend on a write of the Repair, or that an item reads, is still left
by default, with the note that says so.
