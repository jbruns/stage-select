# 17. A Game's status counts only its own files

Date: 2026-11-11

## Status

Accepted

## Context

Redump DATs list many byte-identical tracks under different Games. At the PlayStation acceptance, 62 Games audited as
Incomplete because one track matched a file inside another Game's ZIP: per ROM it was accurate, but nothing about the
Game itself was present or playable. Game Status is meant to tell the Operator what they can play.

## Decision

- An Entry inside a container named for another Game (that Game's ZIP or folder) does not make a ROM present for this
  Game. It makes the ROM Available Elsewhere.
- Game Status counts only present ROMs. A Game whose ROMs are all absent or Available Elsewhere is Missing. The Audit
  reports Available Elsewhere ROMs with the Game, and a Repair can still fill the Game from them.
- Per-Entry Findings and Evidence are unchanged: this is about how Entries count towards Games.

A clone's parent's container counts as the clone's own. Merged sets keep a clone's files in its parent's ZIP, and
split sets keep the files a clone shares with its parent there, so treating that container as another Game's would
make every complete clone in such a set Missing. The DAT's `cloneof` names the parent.

## Amendment (2026-11-25): a file named for no Game counts for the one Game it singles out

At the PlayStation acceptance, a Source Set held a duplicate of a Game under an older Redump name, a ZIP named for no
Game. Each of its Entries counted towards every ROM its payload matched, so its common audio track made about 90 other
Games Incomplete, though nothing of those Games was there.

- An Entry inside a ZIP or folder named for no Game counts towards the ROMs its Finding gives, of one Game alone, when
  that file's Entries single out one Game: the Games of the Entries whose ROMs are all of one Game are that one Game.
  In the example, Track 1 matches only the renamed Game, so Track 2 counts only for it.
- When they single out no Game, or more than one, each Entry counts towards every ROM its Finding gives, as before.
- A loose file in the Source Set's own folder keeps counting towards every ROM its Finding gives: it has no other
  Entries to tell its Game by.
- Per-Entry Findings and Evidence are unchanged: an Entry's ROMs still list every ROM its payload matches. Available
  Elsewhere is unchanged too: it is only for an Entry inside another Game's ZIP or folder.

The Audit settles this once every Entry is in, as it settles an unnamed Entry in a Game's own ZIP: one pass over such
Entries, no read more.

A Repair sets such a duplicate aside by default when every ROM it holds is also in a Game's own file that stays as it
is, and nothing in the Repair reads it; see ADR 0026's amendment of the same date.

An Entry that counts towards one Game alone names, in `complete_in`, the Game and the other file named for it whose
Matched Entries alone make it Complete: the ZIP, the folder of loose files, or the loose file in the Source Set's own
folder. Of several such files, it names the first counted. Its Finding stays as it is (Misnamed, for the duplicate in
the example); `complete_in` is extra evidence that the Entry is a duplicate. An Entry that counts towards several Games,
or towards a Game no other such file makes Complete, names none.
