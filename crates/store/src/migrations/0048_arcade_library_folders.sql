-- MAME Library Folders (#252, ADR 0026). A Build records a selected Game it skipped because a Dependency's ROMs, such
-- as a device's, have no source as `missing_dependency`.
CREATE TABLE build_incomplete_new (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    reason TEXT NOT NULL CHECK (
        reason IN ('unresolved', 'no_source', 'failed', 'conflict', 'blocked', 'missing_dependency')
    ),
    grp TEXT NOT NULL,
    game TEXT NOT NULL,
    target TEXT NOT NULL,
    detail TEXT,
    PRIMARY KEY (build_id, seq)
);
INSERT INTO build_incomplete_new SELECT build_id, seq, reason, grp, game, target, detail FROM build_incomplete;
DROP TABLE build_incomplete;
ALTER TABLE build_incomplete_new RENAME TO build_incomplete;

-- A built file that is a Disk of an Arcade DAT's Game, `<game>/<disk>.chd`, copied from its Source Set: its folder is
-- one a Library Folder's page lists for RomM to leave out.
ALTER TABLE built_files ADD COLUMN disk INTEGER NOT NULL DEFAULT 0;
