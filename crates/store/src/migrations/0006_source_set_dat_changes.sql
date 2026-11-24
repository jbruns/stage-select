-- Each change of a Source Set's DAT, made in place by actor at changed_at: from the DAT it was bound to, to the one it
-- is bound to since. source_sets.dat_id is the latest change's to_dat_id. What the Source Set did before keeps the DAT
-- Revision it used: Audits, Selections, Builds and kept files each name their own.
CREATE TABLE source_set_dat_changes (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    from_dat_id INTEGER NOT NULL REFERENCES dats (id),
    to_dat_id INTEGER NOT NULL REFERENCES dats (id),
    actor TEXT NOT NULL,
    changed_at TEXT NOT NULL
);
CREATE INDEX source_set_dat_changes_by_set ON source_set_dat_changes (source_set_id, id);
