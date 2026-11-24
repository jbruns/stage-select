-- Each game the Operator removed from a Library Folder, by its name in the DAT or an Addition's name: every plan for
-- the folder leaves it out as if its Selection hadn't selected it, until the Operator restores it. Added in the
-- transaction that approves plan_id, the plan that removed it, with what that plan did with its files: 'send_back'
-- (a Placed Game returned to its Source Set), 'set_aside' (kept files) or 'delete' (kept, then deleted).
CREATE TABLE removed_games (
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    game TEXT NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('send_back', 'set_aside', 'delete')),
    plan_id INTEGER NOT NULL REFERENCES plans (id),
    removed_at TEXT NOT NULL,
    PRIMARY KEY (folder_id, game)
);

-- The deletion of the files an approved plan keeps aside of the games it removes to delete: recorded in the
-- transaction that approves plan_id, by actor, and carried out once its Build has kept them aside, as an approved
-- deletion of kept files by the same actor, of the kept files that Build recorded under the plan's removals' names.
-- finished_at is set once that deletion is approved, or once nothing is left to delete. One the service stopped
-- before finishing is finished when it next starts.
CREATE TABLE removal_deletions (
    plan_id INTEGER PRIMARY KEY REFERENCES plans (id),
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    actor TEXT NOT NULL,
    finished_at TEXT
);
