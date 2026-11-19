-- Single Copy (#216, ADR 0023): Place Converted Games, placing a Game the Library Folder stores as CHD.

-- Whether a Build under Single Copy places a Game its Play Library stores in a format no Source Set holds, CHD, rather
-- than copying it: play libraries only, as single_copy is; null for a system that follows the default.
ALTER TABLE storage_policy ADD COLUMN place_converted INTEGER;

-- A Placed Game's file that is a CHD: chd_sha1, the SHA-1 its header held when extracting it gave back every track
-- of its disc exactly, which a quick inventory takes as Claim Evidence; chd_roms, its disc's ROMs as the DAT listed
-- them then, JSON as a plan item's, its cue sheet among them for a CD; and cue_sha256, for a CD, the DAT-exact cue
-- sheet a CHD doesn't hold, kept in the store under cues/<cue_sha256>. Null for every other placed file.
ALTER TABLE placed_files ADD COLUMN chd_sha1 TEXT;
ALTER TABLE placed_files ADD COLUMN chd_roms TEXT;
ALTER TABLE placed_files ADD COLUMN cue_sha256 TEXT;

-- The cue sheet a placing item converted to CHD keeps in the store, by SHA-256: recorded in the transaction that
-- commits its unit, once the blob is written and synced, so that it is held from then until its Game is placed, and
-- from then by its placed file. Null for every other item.
ALTER TABLE build_items ADD COLUMN cue_sha256 TEXT;

-- A Game that needs review because a Build proved its placed CHD doesn't hold its disc as the DAT lists it: extracting
-- it gave back other bytes, or chdman said it is damaged. A current condition of the Game, kept apart from every Audit,
-- whose verdicts record only what each read: raised by the Build build_id, about its placed file path as Audits name
-- it, with why, and cleared once an Audit verifies the Game by payload or a return verifies it home.
CREATE TABLE review_flags (
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    game TEXT NOT NULL,
    path TEXT NOT NULL,
    reason TEXT NOT NULL,
    build_id INTEGER NOT NULL REFERENCES builds (id),
    raised_at TEXT NOT NULL,
    PRIMARY KEY (source_set_id, game)
);
