# 3. SQLite store with content-addressed blobs

Date: 2026-10-28

## Status

Accepted

## Context

The Service needs durable DAT Revisions, a Source Set registry, Audits, jobs and, later, Selections and Change
Plans. Jobs and Change Plans need transactional state changes that survive a restart. Imported DATs must stay
byte-exact. The UI filters and pages Audit Entries in real time, and an Audit of a Redump or MAME set can hold over
100,000 Entries. The alternatives were plain JSON files, which have no transactions or indexed queries, and an external
Postgres, which adds a Service to run and back up for a single-Operator application.

## Decision

- One SQLite database in the Service's data volume. Blobs, starting with unmodified DAT files, are stored beside it
  under `dats/<sha256>`, addressed by their SHA-256.
- WAL mode, with one writer connection that serialises writes and a pool of readers. Every connection sets
  `synchronous=FULL`, `foreign_keys=ON` and a `busy_timeout`.
- A blob is written to a temp file in the store directory, fsynced, renamed into place, and its directory fsynced, all
  before the row that refers to it is inserted. A blob without a row is garbage; a row without its blob cannot occur.
- Each job state transition is one transaction. An Audit's Entries and Game Statuses are rows, written in one
  transaction when the Audit finishes, so a partial Audit is never visible. The versioned JSON report is generated from
  those rows.
- Migrations are embedded and forward-only. Each runs at startup in a transaction, after a `VACUUM INTO` backup.
- A DAT Revision's blob is kept while a retained Audit, a Selection or a Change Plan refers to it, or while it is the
  newest imported revision of its DAT. Otherwise it is deleted. The DAT Revision's row stays, with its SHA-256,
  header and provenance, so history still names it. Importing the same bytes again restores the blob.
- Backups use SQLite's online backup, never a raw copy of the database files.

## Consequences

- The database and its blobs must live on one volume that supports fsync and atomic rename.
- One writer bounds write throughput. That is enough for one Operator, and readers never block it in WAL mode.

## Amendment (2026-10-29, #9)

- One process at a time opens the store, holding an exclusive lock on `<store>/lock`: the Service (#10).
- A blob restored by importing its bytes again is also kept until an Audit using it finishes or a newer revision of
  its DAT is imported.

## Amendment (2026-11-15): explicit DAT Revision deletion

Accepted; implemented in #168.

Automatic blob pruning alone leaves unused DAT Revisions in the catalogue, including the newest revision of a DAT
that no Source Set uses. The Operator may explicitly delete an individual DAT Revision after confirmation, subject
to these constraints:

- Any registered Source Set for its DAT blocks explicit deletion of every revision of that DAT, not only the
  revision of its latest Audit.
- Retained Audits, Selections, Change Plans, Builds and active work that still need the revision also block deletion.
  The Service reports the blockers; deletion never purges dependencies to make a revision eligible.
- An eligible deletion removes the revision from the catalogue and deletes its blob if present, even when it is the
  newest, final or restored revision. Historical metadata stays where needed; no game files are touched. A DAT with
  no remaining catalogue revisions disappears from the catalogue.
- Automatic pruning otherwise remains unchanged: it removes eligible blobs, not catalogue entries. A browser
  download serves only the unmodified retained blob; a pruned file is unavailable, never regenerated or fetched as
  a substitute.

This makes explicit deletion an exception to the newest-revision and restored-blob holds above, without weakening
the retention required by dependent work or history.

Implementation notes (#168): the blockers, and which references are only history, are listed in
`crates/store/src/dat_deletion.rs`; a registered Additions Set for the DAT blocks too, as a Source Set does. History
tables refer to a revision through non-null foreign keys, so a deleted revision that history still names keeps its
row, marked `deleted_at` and holding no blob: it is in no catalogue listing, never the newest of its DAT, never
chosen for new work, and history keeps its identity. One nothing names is deleted outright. Importing its bytes again
is a fresh import into the same row, taking a new `import_seq`, so among revisions whose headers order them equally it
is the newest, as any fresh import is. A MAME release whose fetched revision was deleted no longer counts as fetched.
Eligibility is checked again in the writer transaction that deletes.
