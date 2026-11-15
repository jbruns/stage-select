-- The latest comparison of RomM's view of a Library Folder with the files its Builds made (#143, ADR 0020): one per
-- folder, replaced each time. `error` says why one couldn't be done, and then it has no counts or rows. `rows` is
-- JSON: each built file RomM sees otherwise, with its flags. Its ROM ids are RomM's at `base_url` for the platform
-- `fs_slug`, so a rescan is sent only while both are still RomM's address and the folder's platform.
CREATE TABLE romm_reconciliations (
    folder_id INTEGER PRIMARY KEY REFERENCES library_folders (id),
    job_id INTEGER REFERENCES jobs (id),
    base_url TEXT NOT NULL,
    fs_slug TEXT NOT NULL,
    reconciled_at TEXT NOT NULL,
    error TEXT,
    files INTEGER NOT NULL,
    clean INTEGER NOT NULL,
    unidentified INTEGER NOT NULL,
    title_mismatch INTEGER NOT NULL,
    hash_mismatch INTEGER NOT NULL,
    rows TEXT NOT NULL
);
