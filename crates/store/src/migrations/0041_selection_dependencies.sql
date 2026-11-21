-- The arcade selector (#249, ADRs 0025 and 0027): a Selection lists each Dependency of its Winners once, in a role of
-- its own, with the Winners that need it. SQLite can't change a CHECK, so selection_releases is rebuilt with the
-- dependency role and needed_by, a JSON array of the Winners' names for a dependency and null for every other role.
CREATE TABLE selection_releases_new (
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    seq INTEGER NOT NULL,
    group_name TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('winner', 'alternative', 'exclusion', 'dependency')),
    title TEXT NOT NULL,
    clone_of TEXT,
    reason TEXT,
    stage TEXT NOT NULL,
    ref TEXT,
    message TEXT NOT NULL,
    unresolved INTEGER NOT NULL,
    from_choice INTEGER NOT NULL,
    needed_by TEXT,
    PRIMARY KEY (selection_id, seq),
    CHECK ((role = 'dependency') = (needed_by IS NOT NULL))
);
INSERT INTO selection_releases_new (selection_id, seq, group_name, role, title, clone_of, reason, stage, ref, message,
                                    unresolved, from_choice)
SELECT selection_id, seq, group_name, role, title, clone_of, reason, stage, ref, message, unresolved, from_choice
FROM selection_releases;
DROP TABLE selection_releases;
ALTER TABLE selection_releases_new RENAME TO selection_releases;
CREATE INDEX selection_releases_by_role ON selection_releases (selection_id, role);
