-- Single Copy (#214, ADR 0023): builds that place a Source Set's Games in the Library Folder holding them.

-- Whether a Play Library holding its Source Set's Placed Games places each selected Game rather than copying it:
-- play libraries only, with per-system overrides; null follows the default, which is off.
ALTER TABLE storage_policy ADD COLUMN single_copy INTEGER CHECK (single_copy IN (0, 1));

-- A placed file's SHA-256 is null when it was placed by a rename, which reads nothing: its Audit's payload Evidence
-- stands for it, and a check trusts it only while its stamp is as recorded. Rebuilt to drop NOT NULL; no row changes.
CREATE TABLE placed_files_new (
    folder_id INTEGER NOT NULL,
    path TEXT NOT NULL,
    source_set_id INTEGER NOT NULL,
    game TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    sha256 TEXT,
    PRIMARY KEY (folder_id, path),
    FOREIGN KEY (source_set_id, game, folder_id) REFERENCES placed_games (source_set_id, game, folder_id)
);
INSERT INTO placed_files_new SELECT folder_id, path, source_set_id, game, size, mtime_ns, dev, ino, ctime_ns, sha256
    FROM placed_files;
DROP TABLE placed_files;
ALTER TABLE placed_files_new RENAME TO placed_files;
CREATE INDEX placed_files_by_game ON placed_files (source_set_id, game);

-- A Build item that places its Game, of kind 'place', is recorded before each step it takes so that recovery rolls it
-- forward or back: place_state is 'renaming' before its Source Set file is renamed into the Library Folder; 'copying'
-- before it is copied there instead (crate::carry, with the copy_* intent as repair_steps records it); 'converting'
-- before the file converted from it is written; 'disposing' once the Game is recorded placed, before each original
-- is deleted; then 'done'. Null for every other item, and for one not begun. Rebuilt to allow the kind; no row
-- changes. A file a placement puts in the folder is recorded built, of kind 'place', as well as placed.
CREATE TABLE build_items_new (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    target TEXT NOT NULL,
    game TEXT NOT NULL,
    kind TEXT NOT NULL
        CHECK (kind IN ('copy', 'repack', 'adopt', 'remove', 'keep_aside', 'acknowledge', 'restore', 'place')),
    state TEXT NOT NULL CHECK (state IN ('pending', 'publishing', 'applied', 'failed', 'uncertain')),
    error TEXT,
    sha256 TEXT,
    size INTEGER,
    dev INTEGER,
    ino INTEGER,
    origin TEXT,
    kept_id INTEGER REFERENCES kept_files (id),
    chd_sha1 TEXT,
    chd_raw_sha1 TEXT,
    mtime_ns INTEGER,
    unit TEXT,
    place_state TEXT CHECK (place_state IN ('renaming', 'copying', 'converting', 'disposing', 'done')),
    copy_staged TEXT,
    copy_dev INTEGER,
    copy_ino INTEGER,
    copy_sha256 TEXT,
    PRIMARY KEY (build_id, seq)
);
INSERT INTO build_items_new (build_id, seq, target, game, kind, state, error, sha256, size, dev, ino, origin, kept_id,
                             chd_sha1, chd_raw_sha1, mtime_ns, unit)
SELECT build_id, seq, target, game, kind, state, error, sha256, size, dev, ino, origin, kept_id, chd_sha1,
       chd_raw_sha1, mtime_ns, unit
FROM build_items;
DROP TABLE build_items;
ALTER TABLE build_items_new RENAME TO build_items;

CREATE TABLE built_files_new (
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    target TEXT NOT NULL,
    game TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('copy', 'repack', 'adopt', 'place')),
    build_id INTEGER NOT NULL REFERENCES builds (id),
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    roms TEXT NOT NULL,
    sources TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    sha256 TEXT,
    torrentzip INTEGER NOT NULL DEFAULT 1,
    loose INTEGER NOT NULL DEFAULT 0,
    addition_id INTEGER REFERENCES additions (id),
    chd INTEGER NOT NULL DEFAULT 0,
    chd_sha1 TEXT,
    chd_raw_sha1 TEXT,
    PRIMARY KEY (folder_id, target)
);
INSERT INTO built_files_new SELECT folder_id, target, game, kind, build_id, selection_id, audit_id, dat_revision_id,
       roms, sources, size, mtime_ns, sha256, torrentzip, loose, addition_id, chd, chd_sha1, chd_raw_sha1
FROM built_files;
DROP TABLE built_files;
ALTER TABLE built_files_new RENAME TO built_files;
CREATE INDEX built_files_by_dat_revision ON built_files (dat_revision_id);

-- Each Source Set file a placing Build item consumes, by its path relative to the Source Set's folder, as the plan
-- found it: deleted once the Game is placed, under ADR 0010's trusted-stamp rule. sha256 is what it was read as while
-- it held still, recorded before anything is written for it; slot and moved_ctime_ns its place in the Source Set's
-- private .deleting folder, recorded before it is moved there, and the change time read through the descriptor held
-- across that move. state: 'pending', 'deleted', or 'kept' when it couldn't be proven unchanged and was put back.
CREATE TABLE placement_originals (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    n INTEGER NOT NULL,
    path TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    sha256 TEXT,
    slot TEXT,
    moved_ctime_ns TEXT,
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'deleted', 'kept')),
    detail TEXT,
    PRIMARY KEY (build_id, seq, n)
);
