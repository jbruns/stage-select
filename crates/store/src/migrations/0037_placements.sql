-- Single Copy (#213, ADR 0023): Placements, and the Library Folder that holds each Source Set's Placed Games.

-- The Library Folder, bound to the Source Set, that holds its Placed Games. Null names none: then the only registered
-- Library Folder bound to it holds them, if there is just one.
ALTER TABLE source_sets ADD COLUMN holding_folder_id INTEGER REFERENCES library_folders (id);

-- Each Placed Game: a Game of a Source Set, by its name in the DAT, whose Placement is the Library Folder folder_id. A
-- Game with no row is in the Source Set's own folder. It stays the Source Set's Game: its Audits read it there.
CREATE TABLE placed_games (
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    game TEXT NOT NULL,
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    placed_at TEXT NOT NULL,
    PRIMARY KEY (source_set_id, game),
    UNIQUE (source_set_id, game, folder_id)
);
CREATE INDEX placed_games_by_folder ON placed_games (folder_id);

-- Each file of a Placed Game, by its path relative to its Library Folder, laid out as the Storage Policy says, with
-- the size, modification time and SHA-256 it had when it was placed or a Repair last moved it: what a check of the
-- folder compares it with. An Audit reads only these files of the folder.
CREATE TABLE placed_files (
    folder_id INTEGER NOT NULL,
    path TEXT NOT NULL,
    source_set_id INTEGER NOT NULL,
    game TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    PRIMARY KEY (folder_id, path),
    FOREIGN KEY (source_set_id, game, folder_id) REFERENCES placed_games (source_set_id, game, folder_id)
);
CREATE INDEX placed_files_by_game ON placed_files (source_set_id, game);
