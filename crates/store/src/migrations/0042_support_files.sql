-- MAME Support Files (ADR 0012's amendment): progetto-SNAPS's catver.ini, languages.ini and bestgames.ini, each kept
-- unmodified under `support_files/<sha256>` like a DAT Revision, fetched or dropped in. None is ever deleted: a
-- Selection records the ones it read.
-- kind: catver, languages or bestgames, as its header names it.
-- mame_version: the MAME version its `;; … MAME 0.NNN ;;` header names, such as `0.289`.
-- commit_sha: the mirror commit it was first fetched at; null for one dropped in.
CREATE TABLE support_files (
    id INTEGER PRIMARY KEY,
    sha256 TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('catver', 'languages', 'bestgames')),
    file_name TEXT NOT NULL,
    mame_version TEXT NOT NULL,
    commit_sha TEXT,
    origin TEXT NOT NULL CHECK (origin IN ('import', 'fetch')),
    source_url TEXT,
    imported_at TEXT NOT NULL
);

-- A request of the Support Files' system: the commit it found or fetched at, the Support File it brought, and the
-- ListXML DAT Revision it was sent for, with that revision's import_seq when it was sent. The Support Files are
-- fetched for a ListXML revision once each file has a success sent for it or for a revision imported after it; the
-- import_seq is kept with the fetch because a revision deleted and imported again takes a new one.
ALTER TABLE fetches ADD COLUMN commit_sha TEXT;
ALTER TABLE fetches ADD COLUMN support_sha256 TEXT;
ALTER TABLE fetches ADD COLUMN for_revision_id INTEGER REFERENCES dat_revisions (id);
ALTER TABLE fetches ADD COLUMN for_import_seq INTEGER;
