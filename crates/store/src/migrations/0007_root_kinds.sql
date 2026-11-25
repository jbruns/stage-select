-- A Root's kind says what it holds, and fixes whether it is writable (#351): 'general' splits into 'sources', for
-- Source Sets, which is read-only but for the writable_paths each Source Set's switch lists, and 'library', for Play
-- Library Folders, which is writable, as are 'ingest' and 'additions'. A 'general' Root becomes 'sources' if it was
-- read-only and 'library' if it was writable. Each change recorded keeps the Root as it became, its kind mapped the
-- same way. SQLite can't change a CHECK, so the table is rebuilt; no other table refers to it.
CREATE TABLE roots_new (
    name TEXT PRIMARY KEY CHECK (name <> ''),
    path TEXT NOT NULL CHECK (path <> ''),
    kind TEXT NOT NULL CHECK (kind IN ('sources', 'library', 'ingest', 'additions')),
    writable INTEGER NOT NULL CHECK (writable IN (0, 1)),
    writable_paths TEXT NOT NULL DEFAULT '[]',
    readers INTEGER,
    read_mib_per_s INTEGER,
    write_mib_per_s INTEGER,
    updated_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (writable = (kind <> 'sources')),
    CHECK (writable = 0 OR writable_paths = '[]')
);
INSERT INTO roots_new (name, path, kind, writable, writable_paths, readers, read_mib_per_s, write_mib_per_s,
                       updated_by, updated_at)
SELECT name, path,
       CASE WHEN kind <> 'general' THEN kind WHEN writable = 1 THEN 'library' ELSE 'sources' END,
       writable, writable_paths, readers, read_mib_per_s, write_mib_per_s, updated_by, updated_at
FROM roots;
DROP TABLE roots;
ALTER TABLE roots_new RENAME TO roots;
CREATE UNIQUE INDEX roots_one_additions ON roots (kind) WHERE kind = 'additions';

UPDATE root_changes
SET root_after = json_set(root_after, '$.kind',
                          CASE WHEN json_extract(root_after, '$.writable') THEN 'library' ELSE 'sources' END)
WHERE root_after IS NOT NULL AND json_extract(root_after, '$.kind') = 'general';
