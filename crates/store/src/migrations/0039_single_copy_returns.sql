-- Single Copy (#215, ADR 0023): returning Placed Games to their Source Set's folder.

-- A Build item of kind 'return' takes one release placed in the Library Folder back to its Source Set's folder, whole,
-- and records each step before it takes it so that recovery rolls it forward or back. return_state is 'renaming'
-- before its placed files are renamed home; 'writing' before the files it writes home are staged in the Source Set's
-- folder; 'publishing' once every one is staged and verified, before any is put in place; 'aside' before the placed
-- files that go are moved into the Library Folder's private .deleting folder; 'committed' once the release is recorded
-- home, before those are deleted; then 'done'. Until it is committed every step is rolled back, and the release stays
-- placed; once it is, every step is rolled forward. Null for every other item, and for one not begun. Rebuilt to allow
-- the kind; no row changes.
CREATE TABLE build_items_new (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    target TEXT NOT NULL,
    game TEXT NOT NULL,
    kind TEXT NOT NULL
        CHECK (kind IN ('copy', 'repack', 'adopt', 'remove', 'keep_aside', 'acknowledge', 'restore', 'place',
                        'return')),
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
    place_chain TEXT,
    return_state TEXT
        CHECK (return_state IN ('renaming', 'writing', 'publishing', 'aside', 'committed', 'done')),
    PRIMARY KEY (build_id, seq)
);
INSERT INTO build_items_new (build_id, seq, target, game, kind, state, error, sha256, size, dev, ino, origin, kept_id,
                             chd_sha1, chd_raw_sha1, mtime_ns, unit, place_state, copy_staged, copy_dev, copy_ino,
                             copy_sha256, place_chain)
SELECT build_id, seq, target, game, kind, state, error, sha256, size, dev, ino, origin, kept_id, chd_sha1,
       chd_raw_sha1, mtime_ns, unit, place_state, copy_staged, copy_dev, copy_ino, copy_sha256, place_chain
FROM build_items;
DROP TABLE build_items;
ALTER TABLE build_items_new RENAME TO build_items;

-- Each placed file a return item takes, as the plan found it, by its path relative to the Library Folder: n orders
-- them as the plan does. sha256 is what it was read as, whole, while it held still, before anything was written for it
-- (none for a rename, which reads nothing). keeps says the Library Folder keeps it as a built copy of its Game, which
-- is still selected there. One that goes is moved into the Library Folder's private .deleting folder as slot, recorded
-- first, and its change time read through the descriptor held across that move is moved_ctime_ns; it is deleted once
-- the release is committed, only while it still has that change time and SHA-256. state: 'placed', 'deleted', 'gone'
-- when it was gone before it could be deleted, or 'left' when it could not be proven unchanged and was put back.
CREATE TABLE return_placed (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    n INTEGER NOT NULL,
    path TEXT NOT NULL,
    game TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    keeps INTEGER NOT NULL DEFAULT 0,
    sha256 TEXT,
    slot TEXT,
    moved_ctime_ns TEXT,
    state TEXT NOT NULL DEFAULT 'placed' CHECK (state IN ('placed', 'deleted', 'gone', 'left')),
    detail TEXT,
    PRIMARY KEY (build_id, seq, n)
);

-- Each file a return item puts in the Source Set's folder, by its path relative to it: the name it is staged under in
-- the Source Set's private folder, recorded before it is made, its identity once it is, and its SHA-256 and size once
-- it is verified against the DAT; and chain, the identity of each folder from the Source Set's folder down to the one
-- it goes in, as JSON [[dev, ino], ...], recorded before anything is put in place. A placed file is deleted only once
-- every file of its release is in place below that chain, walked again from the Source Set's registered path with its
-- marker. A rename's file is its placed file n, renamed.
CREATE TABLE return_outputs (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    n INTEGER NOT NULL,
    path TEXT NOT NULL,
    staged TEXT,
    dev INTEGER,
    ino INTEGER,
    sha256 TEXT,
    size INTEGER,
    chain TEXT,
    PRIMARY KEY (build_id, seq, n)
);
