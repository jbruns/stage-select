-- A deletion of kept files of an Ingest Folder, planned for review and carried out once approved, as an Additions
-- Set's is: content binds each file as it was read when the deletion was planned.
CREATE TABLE ingest_kept_deletions (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    content TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    approved_digest TEXT,
    approved_by TEXT,
    approved_at TEXT,
    finished_at TEXT,
    outcomes TEXT,
    recovery_error TEXT
);

-- A kept file of an Ingest Folder is 'kept' until an approved deletion takes it: 'deleting' while deletion_id carries
-- it out, then 'deleted', or 'gone' when it was already missing. A deleted row keeps its name, so no later kept file
-- of its generation takes it.
ALTER TABLE ingest_kept ADD COLUMN state TEXT NOT NULL DEFAULT 'kept'
    CHECK (state IN ('kept', 'deleting', 'deleted', 'gone'));
ALTER TABLE ingest_kept ADD COLUMN deletion_id INTEGER REFERENCES ingest_kept_deletions (id);
ALTER TABLE ingest_kept ADD COLUMN deleted_at TEXT;
ALTER TABLE ingest_kept ADD COLUMN error TEXT;
ALTER TABLE ingest_kept ADD COLUMN slot_ctime_ns TEXT;
