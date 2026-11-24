-- The deletion of a Library Folder an approved plan with delete_folder carries out once its Build has kept aside
-- every built file: recorded in the transaction that approves plan_id, by actor. It deletes the folder's kept files,
-- as an approved deletion of kept files by the same actor, removes the folder when nothing Stage Select didn't write
-- is left in it, and then unregisters it. removing_at is set just before anything of the folder itself is removed,
-- so that a folder found gone after a stop is known to be gone by this deletion. finished_at is set once it is done,
-- with removed (1 when the folder is gone), left (a JSON array of what was left in it) and detail. One the service
-- stopped before finishing is finished when it next starts.
CREATE TABLE folder_deletions (
    plan_id INTEGER PRIMARY KEY REFERENCES plans (id),
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    actor TEXT NOT NULL,
    removing_at TEXT,
    finished_at TEXT,
    removed INTEGER NOT NULL DEFAULT 0,
    left TEXT,
    detail TEXT
);
