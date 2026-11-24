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
