-- RomM's "Additions" collection (#142, ADR 0020): at most one, tracked by RomM's id for it at the address it was
-- made at, never by its name.
CREATE TABLE romm_additions_collection (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    base_url TEXT NOT NULL,
    collection_id INTEGER NOT NULL
);

-- How the collection was kept in step once the Job's scan finished, as JSON; a warning, never the Job's failure.
ALTER TABLE romm_scans ADD COLUMN additions TEXT;

-- The collection to delete once RomM was turned off or its address changed, with the address and token it was made
-- through, until RomM confirms it is gone. At most one: a newer one replaces it. The token is a secret, kept as the
-- saved one is, and never returned or logged.
CREATE TABLE romm_released_collection (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    base_url TEXT NOT NULL,
    token TEXT NOT NULL,
    collection_id INTEGER NOT NULL
);
