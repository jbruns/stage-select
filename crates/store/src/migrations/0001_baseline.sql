-- The baseline schema (ADR 0028): everything a store holds, created at once. Later migrations append to it and are
-- never flattened again. Each table keeps its columns in the order the stores before the baseline had them.

-- Marks the file as a Stage Select store from the baseline on: crate::migrations::APPLICATION_ID, "SSel".
PRAGMA application_id = 1397974380;

------------------------------------------------------------------------------------------------------------------------
-- The store
------------------------------------------------------------------------------------------------------------------------

-- The store's identity, which every Library Folder's marker records, so that a plan can tell a folder this store
-- manages from one another store manages.
CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO meta (key, value) VALUES ('store_id', lower(hex(randomblob(16))));

-- Operational settings, edited in the workbench: one row per section, its value JSON. A section with no row has its
-- defaults. Secrets, such as the webhook's URL and signing secret, are kept here in plain text, and the API never
-- returns them.
CREATE TABLE settings (
    section TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Roots: the folders Stage Select may read, registered by the Operator. A Root's name never changes once registered:
-- every record of a folder in it refers to the Root by name, and keeps doing so after the Root is unregistered.
-- kind: 'ingest' is an Ingest Folder, which is writable; 'additions' holds the Additions Sets, is writable and has no
-- writable_paths, and there is at most one.
-- writable_paths: for a read-only Root, a JSON array of the paths, relative to it, of the Source Sets in it that a
-- repair may write to.
-- readers, read_mib_per_s, write_mib_per_s: the Operator's Ceilings for the Root (ADR 0022); NULL leaves each to the
-- Calibration or the CPU count.
CREATE TABLE roots (
    name TEXT PRIMARY KEY CHECK (name <> ''),
    path TEXT NOT NULL CHECK (path <> ''),
    kind TEXT NOT NULL CHECK (kind IN ('general', 'ingest', 'additions')),
    writable INTEGER NOT NULL CHECK (writable IN (0, 1)),
    writable_paths TEXT NOT NULL DEFAULT '[]',
    readers INTEGER,
    read_mib_per_s INTEGER,
    write_mib_per_s INTEGER,
    updated_by TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (kind = 'general' OR writable = 1),
    CHECK (writable = 0 OR writable_paths = '[]')
);
CREATE UNIQUE INDEX roots_one_additions ON roots (kind) WHERE kind = 'additions';

-- Every change to a Root, oldest first, kept after the Root is unregistered: who made it, when, and the Root as it
-- became (JSON), or NULL for an unregistration.
CREATE TABLE root_changes (
    id INTEGER PRIMARY KEY,
    root TEXT NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('registered', 'edited', 'unregistered')),
    root_after TEXT,
    changed_by TEXT NOT NULL,
    changed_at TEXT NOT NULL
);
CREATE INDEX root_changes_by_root ON root_changes (root, id);

-- Storage Policy: how Source Sets and Play Libraries store each Game, and whether a conversion deletes each original
-- once its converted file is verified. One row per scope and system: system '' is the scope's default, which always
-- sets format and delete_originals; a system's row overrides what it sets, and follows the default where it is NULL.
-- Systems are DATs, as `<provider>/<name>`.
-- format: 'chd' is for play libraries only, as a Source Set holds the DAT's exact bytes (ADR 0013).
-- layout: how a disc system's Play Libraries lay out its discs (ADR 0014); a default with none means 'es_de'.
-- sbi: whether they put each disc's SBI file beside it; only a system's row opts in.
-- single_copy: whether a Play Library holding its Source Set's Placed Games places each selected Game rather than
-- copying it; play libraries only, null follows the default, which is off.
-- place_converted: whether a Build under Single Copy places a Game its Play Library stores in a format no Source Set
-- holds, CHD, rather than copying it; play libraries only, as single_copy is.
-- set_layout: where a MAME system's Source Sets keep a family's shared ROMs, non_merged, split or merged (ADR 0026);
-- source sets only, null follows the default, and for the default means non_merged.
CREATE TABLE storage_policy (
    scope TEXT NOT NULL CHECK (scope IN ('source_sets', 'play_libraries')),
    system TEXT NOT NULL,
    format TEXT CHECK (format IN ('torrent_zip', 'loose', 'loose_folder', 'chd')),
    delete_originals INTEGER CHECK (delete_originals IN (0, 1)),
    updated_by TEXT,
    updated_at TEXT,
    layout TEXT CHECK (layout IN ('es_de', 'flat')),
    sbi INTEGER CHECK (sbi IN (0, 1)),
    single_copy INTEGER CHECK (single_copy IN (0, 1)),
    place_converted INTEGER,
    set_layout TEXT,
    PRIMARY KEY (scope, system),
    CHECK (system <> '' OR (format IS NOT NULL AND delete_originals IS NOT NULL)),
    CHECK (format IS NOT 'chd' OR (scope = 'play_libraries' AND system <> ''))
);
INSERT INTO storage_policy (scope, system, format, delete_originals) VALUES
    ('source_sets', '', 'torrent_zip', 0),
    ('play_libraries', '', 'torrent_zip', 0);

------------------------------------------------------------------------------------------------------------------------
-- DATs and what is fetched for them
------------------------------------------------------------------------------------------------------------------------

-- A DAT, identified by its provider and header name. Each import of new bytes adds a DAT Revision of it.
CREATE TABLE dats (
    id INTEGER PRIMARY KEY,
    provider TEXT NOT NULL,
    name TEXT NOT NULL,
    UNIQUE (provider, name)
);

-- The blob of a DAT Revision is dats/<sha256> beside the database while blob_dropped_at is NULL. restored_at is set
-- when importing its bytes again restores a dropped blob, and holds the blob until an Audit using the revision
-- finishes or a newer revision of its DAT is imported.
--
-- A deleted revision that history still names (a pruned Audit or Selection, a finished Job or Build, a fetch, a
-- followed revision) keeps its row, with deleted_at, hidden from the catalogue, so that history keeps its identity and
-- every foreign key still holds. One nothing names is deleted outright. Importing the same bytes again brings a hidden
-- row back as a fresh import.
CREATE TABLE dat_revisions (
    id INTEGER PRIMARY KEY,
    dat_id INTEGER NOT NULL REFERENCES dats (id),
    sha256 TEXT NOT NULL UNIQUE,
    version TEXT NOT NULL,
    file_name TEXT NOT NULL,
    imported_at TEXT NOT NULL,
    source_url TEXT,
    blob_dropped_at TEXT,
    restored_at TEXT,
    -- Its place among its DAT's revisions, by its header's version, then date: the highest is the newest, whatever
    -- order they were imported in. Renumbered within the DAT on each import.
    header_order INTEGER NOT NULL DEFAULT 0,
    -- The header's date as written; empty if it gives none.
    header_date TEXT NOT NULL DEFAULT '',
    -- The DAT Format: `logiqx`, `listxml` or `software_list`.
    format TEXT NOT NULL DEFAULT 'logiqx',
    -- What the header claims, as written; null when it doesn't say. header_url is the header's own `url`, apart from
    -- the source_url it was downloaded from.
    description TEXT,
    author TEXT,
    homepage TEXT,
    header_url TEXT,
    deleted_at TEXT,
    -- Its import order, apart from its row id: a deleted revision imported again keeps its row, and must still be
    -- newer than every revision imported before it when their headers order them equally.
    import_seq INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX dat_revisions_by_dat ON dat_revisions (dat_id, id);

-- A Redump cue pack: a ZIP of `.cue` sheets for one DAT, kept unmodified under `cue_packs/<sha256>` in the store, with
-- where it came from. A repair fills or replaces a `.cue` ROM from it, matched by the DAT's checksums. origin is
-- 'import' for one the Operator imported, 'fetch' for one fetched. Its file is deleted, and blob_dropped_at set, once
-- no repair plan refers to it and it is not the latest pack for its DAT; the row stays. Importing it again restores it
-- and holds it, as restored_at, until a later pack for its DAT is imported.
CREATE TABLE cue_packs (
    id INTEGER PRIMARY KEY,
    sha256 TEXT NOT NULL UNIQUE,
    dat TEXT NOT NULL,
    file_name TEXT NOT NULL,
    source_url TEXT,
    origin TEXT NOT NULL CHECK (origin IN ('import', 'fetch')),
    cues INTEGER NOT NULL,
    imported_at TEXT NOT NULL,
    blob_dropped_at TEXT,
    restored_at TEXT
);
CREATE INDEX cue_packs_by_dat ON cue_packs (dat);

-- Each `.cue` sheet of a cue pack, hashed once when the pack was imported, so planning a repair matches the DAT's
-- checksums here and reads only the sheet a repair writes.
CREATE TABLE cue_sheets (
    pack_id INTEGER NOT NULL REFERENCES cue_packs (id),
    member TEXT NOT NULL,
    size INTEGER NOT NULL,
    crc TEXT NOT NULL,
    md5 TEXT NOT NULL,
    sha1 TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    PRIMARY KEY (pack_id, member)
);

-- A Redump SBI pack a fetch brought: a ZIP of subchannel files for one DAT, kept unmodified under
-- `sbi_packs/<sha256>`. Its file is deleted, and blob_dropped_at set, once no repair plan refers to it and the latest
-- successful fetch of its system's SBI pack brought another; the row stays.
CREATE TABLE sbi_packs (
    id INTEGER PRIMARY KEY,
    sha256 TEXT NOT NULL UNIQUE,
    dat TEXT NOT NULL,
    file_name TEXT NOT NULL,
    source_url TEXT,
    fetched_at TEXT NOT NULL,
    blob_dropped_at TEXT
);
CREATE INDEX sbi_packs_by_dat ON sbi_packs (dat);

-- MAME Support Files: progetto-SNAPS's catver.ini, languages.ini and bestgames.ini, each kept unmodified under
-- `support_files/<sha256>` like a DAT Revision, fetched or dropped in. None is ever deleted: a Selection records the
-- ones it read.
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

-- Fetching (ADR 0012): every request to a DAT Provider, recorded before it is sent and finished with its outcome. The
-- latest request of each system and artifact holds the next back for a day, whatever its outcome.
-- system: Redump's system code, or for MAME `mame` (its release and ListXML) or a software list's name.
-- outcome: added, unchanged or failed; absent while the request runs, or if the Service stopped during it.
-- pack_sha256: the cue pack (in cue_packs) or SBI pack (in sbi_packs) the request brought.
-- release, asset_url: for MAME, the release found or fetched from, and its ListXML asset.
-- commit_sha, support_sha256, for_revision_id, for_import_seq: for the Support Files' system, the commit it found or
-- fetched at, the Support File it brought, and the ListXML DAT Revision it was sent for, with that revision's
-- import_seq when the fetch chose it. The Support Files are fetched for a ListXML revision once each file has a success
-- sent for it or for a revision imported after it; the import_seq is kept with the fetch because a revision deleted
-- and imported again takes a new one.
CREATE TABLE fetches (
    id INTEGER PRIMARY KEY,
    provider TEXT NOT NULL,
    system TEXT NOT NULL,
    artifact TEXT NOT NULL,
    trigger TEXT NOT NULL,
    actor TEXT NOT NULL,
    url TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    outcome TEXT,
    error TEXT,
    dat_revision_id INTEGER REFERENCES dat_revisions (id),
    pack_sha256 TEXT,
    release TEXT,
    asset_url TEXT,
    commit_sha TEXT,
    support_sha256 TEXT,
    for_revision_id INTEGER REFERENCES dat_revisions (id),
    for_import_seq INTEGER
);
CREATE INDEX fetches_artifact ON fetches (provider, system, artifact, id);

------------------------------------------------------------------------------------------------------------------------
-- Jobs and Calibration
------------------------------------------------------------------------------------------------------------------------

-- A Job: a durable unit of work. Each state change is one transaction. A Job found Running when the Service starts is
-- Interrupted and queued again once, as a new Job with attempt 2; an attempt 2 found Running is Failed instead.
-- An Audit or Selection Job is pinned to a Source Set and a DAT Revision; a Selection Job also runs one Profile
-- version and records the Selection it made. A Build Job carries out an approved plan into a Library Folder, a Check
-- Job reads one; an ingest scan reads an Ingest Folder; a Repair Job carries out its Source Set's approved repair
-- plan; an Additions Job publishes, retires and restores the additions of one Additions Set; a stat walk reads a
-- Source Set with no DAT Revision; a Calibration names no Source Set, DAT Revision or folder.
-- evidence: how an Audit Job's Audit gets its checksums; null for other kinds of Job.
-- cancel_requested_at: when the Operator asked the Job to stop at its next safe point.
-- bytes_read, bytes_written, duration_ms: what the Job read and wrote, and for how long it was metered, recorded as it
-- ends so a finished Job shows its average throughput.
CREATE TABLE jobs (
    id INTEGER PRIMARY KEY,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'interrupted')),
    source_set_id INTEGER REFERENCES source_sets (id),
    dat_revision_id INTEGER REFERENCES dat_revisions (id),
    actor TEXT NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 1,
    requeued_from INTEGER REFERENCES jobs (id),
    audit_id INTEGER REFERENCES audits (id),
    error TEXT,
    readers INTEGER NOT NULL,
    hashers INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    -- Goes up with each change of state, so that a client can tell a stale state from a newer one.
    version INTEGER NOT NULL DEFAULT 1,
    kind TEXT NOT NULL DEFAULT 'audit'
        CHECK (kind IN ('audit', 'selection', 'build', 'check', 'ingest_scan', 'repair', 'additions', 'stat_walk',
                        'calibration')),
    profile_version_id INTEGER REFERENCES profile_versions (id),
    selection_id INTEGER REFERENCES selections (id),
    library_folder_id INTEGER REFERENCES library_folders (id),
    plan_id INTEGER REFERENCES plans (id),
    evidence TEXT CHECK (evidence IN ('quick', 'incremental', 'full')),
    ingest_folder_id INTEGER REFERENCES ingest_folders (id),
    repair_plan_id INTEGER REFERENCES repair_plans (id),
    cancel_requested_at TEXT,
    additions_set_id INTEGER REFERENCES additions_sets (id),
    bytes_read INTEGER,
    bytes_written INTEGER,
    duration_ms INTEGER,
    CHECK ((kind IN ('ingest_scan', 'additions', 'calibration')) = (source_set_id IS NULL AND dat_revision_id IS NULL)),
    CHECK ((kind = 'ingest_scan') = (ingest_folder_id IS NOT NULL)),
    CHECK ((kind = 'repair') = (repair_plan_id IS NOT NULL)),
    CHECK ((kind = 'additions') = (additions_set_id IS NOT NULL)),
    CHECK (kind <> 'stat_walk' OR dat_revision_id IS NULL)
);
CREATE INDEX jobs_by_state ON jobs (state, id);
CREATE INDEX jobs_by_repair_plan ON jobs (repair_plan_id) WHERE repair_plan_id IS NOT NULL;
CREATE INDEX jobs_by_dat_revision ON jobs (dat_revision_id) WHERE dat_revision_id IS NOT NULL;
-- One active Job of each kind per Source Set: an Audit and a Selection may be queued together.
CREATE UNIQUE INDEX jobs_one_active_per_source_set ON jobs (source_set_id, kind)
WHERE state IN ('queued', 'running') AND kind IN ('audit', 'selection', 'repair', 'stat_walk');
-- One active Build or Check per Library Folder: a Check never runs while a Build changes the folder.
CREATE UNIQUE INDEX jobs_one_active_per_folder ON jobs (library_folder_id)
WHERE state IN ('queued', 'running') AND kind IN ('build', 'check');
CREATE UNIQUE INDEX jobs_one_active_per_ingest_folder ON jobs (ingest_folder_id)
WHERE state IN ('queued', 'running') AND kind = 'ingest_scan';
-- One queued Additions Job per Additions Set takes new members; one the Operator cancelled takes none, and does not
-- count.
CREATE UNIQUE INDEX jobs_one_queued_per_additions_set ON jobs (additions_set_id)
WHERE state = 'queued' AND kind = 'additions' AND cancel_requested_at IS NULL;
CREATE UNIQUE INDEX jobs_one_active_calibration ON jobs (kind)
WHERE state IN ('queued', 'running') AND kind = 'calibration';
CREATE TRIGGER jobs_kind_profile BEFORE INSERT ON jobs
WHEN (NEW.kind = 'selection') <> (NEW.profile_version_id IS NOT NULL)
  OR (NEW.kind = 'build') <> (NEW.plan_id IS NOT NULL)
  OR (NEW.kind IN ('build', 'check')) <> (NEW.library_folder_id IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'a selection job runs a profile version, a build job an approved plan, a check job a library folder, an audit job none');
END;

-- Calibration (ADR 0022): one row for each Calibration Job that measured the Host: the CPUs the Service could use
-- then, the curve as JSON (a list of {workers, bytes, duration_ms, bytes_per_s}), and its knee and the throughput
-- there. A Calibration cancelled before its Host sweep finished has none.
CREATE TABLE calibrations (
    job_id INTEGER PRIMARY KEY REFERENCES jobs (id),
    cpus INTEGER NOT NULL,
    curve TEXT NOT NULL,
    knee INTEGER,
    knee_bytes_per_s REAL,
    measured_at TEXT NOT NULL
);

-- One row for each Root a Calibration Job finished measuring, under its name and the path it had then. A Root that had
-- too little to read is 'too_small', one that couldn't be read is 'unreadable', and one in which the sweep couldn't
-- find enough to read in time is 'timed_out', each with no curve points and no knee. available_bytes is what its sweep
-- read.
CREATE TABLE calibration_roots (
    job_id INTEGER NOT NULL REFERENCES jobs (id),
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('measured', 'too_small', 'unreadable', 'timed_out')),
    available_bytes INTEGER NOT NULL,
    curve TEXT NOT NULL,
    knee INTEGER,
    knee_bytes_per_s REAL,
    error TEXT,
    measured_at TEXT NOT NULL,
    PRIMARY KEY (job_id, root)
);
CREATE INDEX calibration_roots_by_root ON calibration_roots (root, job_id);

------------------------------------------------------------------------------------------------------------------------
-- Source Sets
------------------------------------------------------------------------------------------------------------------------

-- A Source Set is a Root plus a path relative to it, displayed by its folder name and bound to a DAT. An Unregistered
-- one keeps its row, and its history, read-only; its name and folder are then free for a new registration, so each
-- uniqueness holds among registrations still registered only. Forgetting it later deletes its row and history.
CREATE TABLE source_sets (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    dat_id INTEGER NOT NULL REFERENCES dats (id),
    added_at TEXT NOT NULL,
    -- Whether the Source Set Follows its DAT: a DAT Revision of it newer than its latest Audit's then makes it Stale.
    -- Following never starts an Audit.
    follow INTEGER NOT NULL DEFAULT 0,
    unregistered_at TEXT,
    -- Coverage (ADR 0021): the regions of its DAT's Games it intends to preserve, as a JSON array of the Selection
    -- engine's region names. Null covers every Game.
    coverage TEXT,
    -- The Library Folder, bound to the Source Set, that holds its Placed Games. Null names none: then the only
    -- registered Library Folder bound to it holds them, if there is just one.
    holding_folder_id INTEGER REFERENCES library_folders (id)
);
CREATE UNIQUE INDEX source_sets_name ON source_sets (name) WHERE unregistered_at IS NULL;
CREATE UNIQUE INDEX source_sets_folder ON source_sets (root, path) WHERE unregistered_at IS NULL;

-- A Source Set a repair may write to. generation counts approved repairs; each keeps files under its own
-- `.stage-select-retained/<generation>/`. marker_at is when its marker was written, by the first approved repair.
CREATE TABLE source_folders (
    source_set_id INTEGER PRIMARY KEY REFERENCES source_sets (id),
    generation INTEGER NOT NULL DEFAULT 0,
    marker_at TEXT,
    -- The first marker's temporary file: marker_temp_pending is committed before it is created, and its identity
    -- once it is, so that recovery writes and publishes that file, and only it.
    marker_temp_pending INTEGER NOT NULL DEFAULT 0,
    marker_temp_dev INTEGER,
    marker_temp_ino INTEGER
);

-- As ingest_dirs, ingest_temps and ingest_temp_names, for the directories a repair creates in a Source Set.
CREATE TABLE source_dirs (
    folder_id INTEGER NOT NULL REFERENCES source_sets (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE source_temps (
    folder_id INTEGER NOT NULL REFERENCES source_sets (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE source_temp_names (
    folder_id INTEGER NOT NULL REFERENCES source_sets (id),
    name TEXT NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- What a stat walk found, compared with the Source Set's latest Audit when it ran, audit_id: how many files' stamps
-- fail the change test, how many files that Audit did not see, and how many it saw that are gone. A walk that finds
-- what the previous one of the Source Set found against the same Audit only moves checked_at and job_id on. A walk
-- that found nothing replaces the earlier walks of its Source Set that found nothing. The Source Set is Stale while
-- its latest walk is against its latest Audit and found something.
CREATE TABLE stat_walks (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    job_id INTEGER REFERENCES jobs (id),
    found_at TEXT NOT NULL,
    checked_at TEXT NOT NULL,
    changed INTEGER NOT NULL,
    added INTEGER NOT NULL,
    removed INTEGER NOT NULL
);
CREATE INDEX stat_walks_by_source_set ON stat_walks (source_set_id, id);

-- Each DAT Revision that made a Following Source Set Stale, when it did: newer than its latest Audit's when it was
-- imported, or when the Operator started following. Its history; whether the Source Set is Stale now is in
-- source_sets.
CREATE TABLE followed_revisions (
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    found_at TEXT NOT NULL,
    PRIMARY KEY (source_set_id, dat_revision_id)
);

------------------------------------------------------------------------------------------------------------------------
-- Audits
------------------------------------------------------------------------------------------------------------------------

-- One row per Audit, inserted as running before its scan so that it holds its DAT Revision, and finished in the
-- transaction that writes its Entries and Game Statuses. A running row left by a crash is deleted on open. A pruned
-- Audit keeps this row: its counts are the summary that remains. finish_seq orders Audits by completion: the latest
-- Audit is the one that finished last, whatever the order they started in.
CREATE TABLE audits (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    status TEXT NOT NULL CHECK (status IN ('running', 'finished')),
    source_set_path TEXT NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    finish_seq INTEGER UNIQUE,
    matched INTEGER NOT NULL DEFAULT 0,
    misnamed INTEGER NOT NULL DEFAULT 0,
    conflicting INTEGER NOT NULL DEFAULT 0,
    mismatched INTEGER NOT NULL DEFAULT 0,
    unmatched INTEGER NOT NULL DEFAULT 0,
    unreadable INTEGER NOT NULL DEFAULT 0,
    complete INTEGER NOT NULL DEFAULT 0,
    incomplete INTEGER NOT NULL DEFAULT 0,
    missing INTEGER NOT NULL DEFAULT 0,
    pruned_at TEXT,
    -- The Job running the Audit, so that failing the Job discards its running Audit in the same transaction.
    job_id INTEGER REFERENCES jobs (id),
    -- How the Audit got its checksums.
    evidence TEXT NOT NULL DEFAULT 'full'
        CHECK (evidence IN ('quick', 'incremental', 'full')),
    -- What the Audit found, as a SHA-256 over its DAT Revision, Entries and Game Statuses, so that a Library Folder is
    -- stale only when a newer Audit of its Source Set found something its Build's did not. Null until it is worked
    -- out, when needed.
    content_sha256 TEXT,
    -- The Header Rule the Audit's DAT Revision named, as the DAT names it (ADR 0016); null when it names none.
    header_rule TEXT,
    -- How many of the Audit's Games carry each Game Warning, kept like the other counts once the Audit is pruned.
    header_missing INTEGER NOT NULL DEFAULT 0,
    -- Nothing to Check (ADR 0025): how many of its Games list no ROM that must be present and can match, counted apart
    -- from Complete, Incomplete and Missing; and how many of those it found any of their own files of, which a placed
    -- count counts against.
    nothing_to_check INTEGER NOT NULL DEFAULT 0,
    nothing_to_check_found INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX audits_by_source_set ON audits (source_set_id, finish_seq);
CREATE INDEX audits_by_dat_revision ON audits (dat_revision_id);

-- Something that cites an Audit as provenance, such as a later Change Plan. A cited Audit is never pruned.
CREATE TABLE audit_citations (
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    cited_by TEXT NOT NULL,
    PRIMARY KEY (audit_id, cited_by)
);

-- roms is a JSON array of {game, name}; algorithms is a comma-separated list in report order.
CREATE TABLE audit_entries (
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    seq INTEGER NOT NULL,
    path TEXT NOT NULL,
    member TEXT,
    finding TEXT NOT NULL,
    roms TEXT NOT NULL,
    named_game TEXT,
    named_rom TEXT,
    evidence_level TEXT,
    algorithms TEXT,
    reason TEXT,
    -- For Payload Evidence, when the payload was last read, and whether the Audit reused that read under the change
    -- test rather than reading it.
    read_at TEXT,
    reused INTEGER NOT NULL DEFAULT 0,
    -- For Payload Evidence, how many bytes of Header the Audit left out of the Entry's checksums; null when none.
    header_bytes INTEGER,
    -- For a CHD's Entry, the Disks it counts for, as JSON in DAT order (ADR 0026); an Unreadable delta CHD records its
    -- parent's combined SHA-1.
    disks TEXT NOT NULL DEFAULT '[]',
    parent_sha1 TEXT,
    PRIMARY KEY (audit_id, seq)
);
CREATE INDEX audit_entries_by_finding ON audit_entries (audit_id, finding);

-- missing_roms is a JSON array of ROM names.
CREATE TABLE audit_games (
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    seq INTEGER NOT NULL,
    name TEXT NOT NULL,
    status TEXT NOT NULL,
    missing_roms TEXT NOT NULL,
    -- A JSON array of the Game's ROMs counted present only on Claim Evidence.
    claimed_roms TEXT NOT NULL DEFAULT '[]',
    -- A JSON array of the Game's Game Warnings.
    warnings TEXT NOT NULL DEFAULT '[]',
    -- A JSON array of the Game's Available Elsewhere ROMs, each with the Games whose ZIP or folder holds it
    -- (ADR 0017).
    available_elsewhere TEXT NOT NULL DEFAULT '[]',
    -- A JSON array of the BIOSes and devices the Game reaches through `romof` and `device_ref`, each with the Game
    -- Status the same Audit found for it (ADR 0025).
    dependencies TEXT NOT NULL DEFAULT '[]',
    -- Its nodump, baddump and optional ROMs, as JSON lists in DAT order. A nodump or optional ROM never lowers a Game
    -- Status.
    not_dumped TEXT NOT NULL DEFAULT '[]',
    bad_dumps TEXT NOT NULL DEFAULT '[]',
    optional_roms TEXT NOT NULL DEFAULT '[]',
    -- Its Disks, as JSON in DAT order (ADR 0026).
    disks TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (audit_id, seq)
);
CREATE INDEX audit_games_by_status ON audit_games (audit_id, status);
-- Availability joins a Selection's releases to an Audit's Game Statuses by name.
CREATE INDEX audit_games_by_name ON audit_games (audit_id, name);

-- The stamp of each file an Audit read in full or reused (unreadable = 0), or could not read (unreadable = 1),
-- recorded by that Audit as it read it, so that a repair acts only on files still as the Audit it is planned from read
-- them, never on a stamp another Audit recorded. An abandoned Audit's are deleted with it.
CREATE TABLE audit_stamps (
    audit_id INTEGER NOT NULL REFERENCES audits (id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    unreadable INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    PRIMARY KEY (audit_id, unreadable, path)
);

-- The stamp of each ZIP archive a quick inventory took claims from, recorded as it opened it, so that a stat walk can
-- tell whether it changed since. Kept apart from audit_stamps, which a repair acts on.
CREATE TABLE audit_claim_stamps (
    audit_id INTEGER NOT NULL REFERENCES audits (id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    PRIMARY KEY (audit_id, path)
);

-- The last read of every payload of a file in a Source Set, by its path relative to the Source Set, kept for later
-- Audits to reuse while the file's dev, ino, size, mtime_ns and ctime_ns are all as they were when it was opened for
-- that read. This change test is a heuristic, not a reread. members is a JSON array of [member, checksums] pairs, in
-- archive order, member null for a loose file. Each file is recorded as soon as it is read, so an Audit cut off part
-- way resumes from what it finished. job_id is the Job that read it: a resumed full Audit reuses only what its
-- interrupted Job read. A file not found by an Audit that finishes is forgotten.
CREATE TABLE payload_reads (
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    path TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    read_at TEXT NOT NULL,
    job_id INTEGER REFERENCES jobs (id),
    members TEXT NOT NULL,
    -- The Header Rule the read was hashed under; null for none. A later Audit reuses the read only under the same
    -- rule.
    header_rule TEXT,
    -- A Disk's read (ADR 0026): the combined SHA-1 `chdman verify` proved a CHD's data hashes to, which an Incremental
    -- Audit reuses while the file's stamp holds. Null for every other file's read.
    disk_sha1 TEXT,
    PRIMARY KEY (source_set_id, path)
);

------------------------------------------------------------------------------------------------------------------------
-- Selection Profiles and Selections (ADR 0001)
------------------------------------------------------------------------------------------------------------------------

-- A named Selection Profile. Its contents live in its versions. A Retired one is hidden from new choices; its versions
-- stay for the Selections that used them.
CREATE TABLE profiles (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    retired_at TEXT
);

-- One immutable version of a Profile: its canonical JSON (compact, maps sorted by key) and that JSON's SHA-256.
-- Versions are numbered 1, 2, … per Profile and never changed or deleted.
CREATE TABLE profile_versions (
    id INTEGER PRIMARY KEY,
    profile_id INTEGER NOT NULL REFERENCES profiles (id),
    version INTEGER NOT NULL,
    json TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    saved_at TEXT NOT NULL,
    saved_by TEXT NOT NULL,
    UNIQUE (profile_id, version)
);
CREATE TRIGGER profile_versions_immutable BEFORE UPDATE ON profile_versions
BEGIN
    SELECT RAISE(ABORT, 'profile versions are immutable');
END;
CREATE TRIGGER profile_versions_kept BEFORE DELETE ON profile_versions
BEGIN
    SELECT RAISE(ABORT, 'profile versions are never deleted');
END;

-- A Selection made for a Source Set from one DAT Revision with one Profile version. It is written whole, in the
-- transaction that finishes its Job. A pruned Selection keeps this row: its summary is what remains, and it no longer
-- holds its DAT Revision's blob.
CREATE TABLE selections (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    profile_version_id INTEGER NOT NULL REFERENCES profile_versions (id),
    job_id INTEGER REFERENCES jobs (id),
    system TEXT NOT NULL,
    system_settings INTEGER NOT NULL,
    -- SelectionPins, SelectionSummary, CoverageDrift and the stale choices of gone groups, as JSON.
    pins TEXT NOT NULL,
    summary TEXT NOT NULL,
    coverage_drift TEXT NOT NULL,
    stale_choices TEXT NOT NULL,
    created_at TEXT NOT NULL,
    pruned_at TEXT,
    -- The Source Set's Coverage the Selection was made under, as a JSON array of the Selection engine's region names
    -- (ADR 0021). Null covered every Game. A Library Folder is Stale when its Source Set's Coverage differs from its
    -- current Build's Selection's.
    coverage TEXT
);
CREATE INDEX selections_by_source_set ON selections (source_set_id, dat_revision_id, id);
CREATE INDEX selections_by_dat_revision ON selections (dat_revision_id);

-- Per Game group: the flags and the Operator's choice state (GroupChoice as JSON), in group order.
CREATE TABLE selection_groups (
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    seq INTEGER NOT NULL,
    name TEXT NOT NULL,
    unresolved INTEGER NOT NULL,
    drift INTEGER NOT NULL,
    choice TEXT,
    PRIMARY KEY (selection_id, seq)
);

-- Per release, in group order and then winners, alternatives and exclusions. Each Dependency of the Winners is listed
-- once, in a role of its own (ADRs 0025 and 0027), with needed_by, a JSON array of the Winners' names that need it;
-- null for every other role.
CREATE TABLE selection_releases (
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
CREATE INDEX selection_releases_by_role ON selection_releases (selection_id, role);

-- Something that cites a Selection as provenance, such as a Library Folder's current Build ('library-folder:<id>'),
-- or holds it for a Build Job from its approval until it ends ('build-job:<id>'); audit_citations does the same for
-- Audits. A cited Selection is never pruned.
CREATE TABLE selection_citations (
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    cited_by TEXT NOT NULL,
    PRIMARY KEY (selection_id, cited_by)
);

------------------------------------------------------------------------------------------------------------------------
-- Library Folders, Change Plans and Builds
------------------------------------------------------------------------------------------------------------------------

-- A Library Folder: a folder in a writable Root that a Source Set's Selections with one Profile are built into. Its
-- system is the Source Set's DAT; a folder may be rebound only to a Source Set of the same DAT. generation counts the
-- Builds that succeeded and is mirrored in the folder's marker. Unregistered as a Source Set is.
CREATE TABLE library_folders (
    id INTEGER PRIMARY KEY,
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    profile_id INTEGER NOT NULL REFERENCES profiles (id),
    generation INTEGER NOT NULL DEFAULT 0,
    current_build_id INTEGER REFERENCES builds (id),
    created_at TEXT NOT NULL,
    unregistered_at TEXT
);
CREATE UNIQUE INDEX library_folders_folder ON library_folders (root, path) WHERE unregistered_at IS NULL;

-- A move of a Library Folder to another path in its Root: planned, approved as reviewed with digest, then carried out
-- by one rename that never replaces anything, so that a stop between the rename and the record is settled, when the
-- Service next starts, by where the folder (dev, ino) is found.
CREATE TABLE library_folder_moves (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    root TEXT NOT NULL,
    from_path TEXT NOT NULL,
    to_path TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    approved_by TEXT,
    approved_at TEXT,
    finished_at TEXT,
    outcome TEXT
);

-- A Change Plan: what building one Selection into a Library Folder would do, as PlanContent JSON, and the SHA-256 of
-- that JSON, which an approval binds to. changes is the PlanChange JSON against previous_selection_id. check_id is the
-- check a cleanup plan acts on.
CREATE TABLE plans (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    audit_id INTEGER REFERENCES audits (id),
    generation INTEGER NOT NULL,
    content TEXT NOT NULL,
    digest TEXT NOT NULL,
    previous_selection_id INTEGER REFERENCES selections (id),
    changes TEXT NOT NULL,
    created_at TEXT NOT NULL,
    check_id INTEGER REFERENCES folder_checks (id)
);
CREATE INDEX plans_by_folder ON plans (folder_id, id);

CREATE TABLE plan_approvals (
    plan_id INTEGER PRIMARY KEY REFERENCES plans (id),
    digest TEXT NOT NULL,
    actor TEXT NOT NULL,
    approved_at TEXT NOT NULL
);

-- A Build: an approved plan carried out by a Build Job, from the generation in its plan to generation + 1. kind is a
-- Build of a Selection ('build'), or a cleanup of a folder's drift ('cleanup'). Only a Build that succeeded settles an
-- earlier one that stopped: a cleanup never does.
CREATE TABLE builds (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    plan_id INTEGER NOT NULL REFERENCES plans (id),
    job_id INTEGER NOT NULL UNIQUE REFERENCES jobs (id),
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    state TEXT NOT NULL CHECK (state IN ('running', 'succeeded', 'failed', 'interrupted')),
    generation INTEGER NOT NULL,
    started_at TEXT NOT NULL,
    finished_at TEXT,
    -- Set in the transaction that advances the folder to `generation` and makes this its current Build, which
    -- commits before the marker is advanced: the only evidence on which recovery moves a marker one generation
    -- behind the store forward. A marker is never moved back.
    finalizing_at TEXT,
    -- When the folder was last left clean after the Build: no staging directory, no half-written marker, every
    -- publishing item settled. A Build that ended without it still needs recovery, and its folder is not planned.
    cleaned_at TEXT,
    -- Why recovery left the folder as it was.
    recovery_error TEXT,
    kind TEXT NOT NULL DEFAULT 'build' CHECK (kind IN ('build', 'cleanup'))
);
CREATE INDEX builds_by_folder ON builds (folder_id, id);
-- What finds the work and history depending on a DAT Revision, so listing what blocks each revision's deletion stays
-- quick as history grows.
CREATE INDEX builds_by_plan ON builds (plan_id);
CREATE INDEX builds_by_dat_revision ON builds (dat_revision_id);

-- Each file a Build writes, recorded pending before the first is written, so that an interrupted Build shows what it
-- never reached. An item is 'uncertain' when recovery found something other than its file at its target. An 'adopt'
-- item records a file in conflict as built, writing nothing; a 'remove' item keeps aside a built file no longer
-- selected. An item that moves an entry within the folder, rather than publishing a staged file, records where it
-- moves it from ('origin', relative to the folder) with the entry's identity as its intent, before it is moved; a
-- restore also names the kept file it moves ('kept_id').
--
-- An item of kind 'place' (ADR 0023) is recorded before each step it takes so that recovery rolls it forward or back:
-- place_state is 'renaming' before its Source Set file is renamed into the Library Folder; 'copying' before it is
-- copied there instead (crate::carry, with the copy_* intent as repair_steps records it); 'converting' before the file
-- converted from it is written; 'disposing' once the Game is recorded placed, before each original is deleted; then
-- 'done'. place_chain is the identity of each folder from the Library Folder down to the folder a placed file goes in,
-- as JSON [[dev, ino], ...], recorded before anything of its Game moves; the Source Set's file is consumed only once
-- that chain, walked again from the folder's registered path with its marker, is unchanged.
--
-- An item of kind 'return' takes one release placed in the Library Folder back to its Source Set's folder, whole.
-- return_state is 'renaming' before its placed files are renamed home; 'writing' before the files it writes home are
-- staged in the Source Set's folder; 'publishing' once every one is staged and verified, before any is put in place;
-- 'aside' before the placed files that go are moved into the Library Folder's private .deleting folder; 'committed'
-- once the release is recorded home, before those are deleted; then 'done'. Until it is committed every step is
-- rolled back, and the release stays placed; once it is, every step is rolled forward.
--
-- place_state and return_state are null for every other item, and for one not begun.
CREATE TABLE build_items (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    target TEXT NOT NULL,
    game TEXT NOT NULL,
    kind TEXT NOT NULL
        CHECK (kind IN ('copy', 'repack', 'adopt', 'remove', 'keep_aside', 'acknowledge', 'restore', 'place',
                        'return')),
    state TEXT NOT NULL CHECK (state IN ('pending', 'publishing', 'applied', 'failed', 'uncertain')),
    error TEXT,
    -- The staged file's SHA-256, size, device and inode, recorded with state 'publishing' before it is renamed into
    -- place, so that the Build and recovery can tell the file it put there from any other.
    sha256 TEXT,
    size INTEGER,
    dev INTEGER,
    ino INTEGER,
    origin TEXT,
    kept_id INTEGER REFERENCES kept_files (id),
    -- The SHA-1s of the CHD an item verified and is putting in place, recorded with its publish intent so that
    -- recovery records it built as the Build would have.
    chd_sha1 TEXT,
    chd_raw_sha1 TEXT,
    -- The modification time of the file an item verified and is putting in place, with its SHA-256, size, device and
    -- inode: the stamp a CHD's converted original is deleted against, since a CHD is not read again whole once
    -- verified.
    mtime_ns INTEGER,
    -- The release whose discs an item is put in place with, as one unit: once any is renamed into place, recovery puts
    -- the rest in place from their staged files, so that a release is never left split across formats. Null for an
    -- item put in place alone.
    unit TEXT,
    place_state TEXT CHECK (place_state IN ('renaming', 'copying', 'converting', 'disposing', 'done')),
    copy_staged TEXT,
    copy_dev INTEGER,
    copy_ino INTEGER,
    copy_sha256 TEXT,
    place_chain TEXT,
    return_state TEXT
        CHECK (return_state IN ('renaming', 'writing', 'publishing', 'aside', 'committed', 'done')),
    -- The cue sheet a placing item converted to CHD keeps in the store, by SHA-256: recorded in the transaction that
    -- commits its unit, once the blob is written and synced, so that it is held from then until its Game is placed,
    -- and from then by its placed file. Null for every other item.
    cue_sha256 TEXT,
    PRIMARY KEY (build_id, seq)
);

-- Each unit's progress, durably: 'staged' while its discs are written and checked, when a failure, a cancel or a stop
-- rolls it back; 'committing' once, with every disc's publish intent and no cancel asked for, it may keep files aside
-- and rename its discs into place, which recovery then finishes; 'done' or 'rolled_back' after.
CREATE TABLE build_units (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    unit TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('staged', 'committing', 'done', 'rolled_back')),
    PRIMARY KEY (build_id, unit)
);

-- What a Build left its folder lacking of its Selection, recorded when it succeeds: each selected Game skipped as
-- unresolved, with no source, blocked or in conflict, or because a Dependency's ROMs, such as a device's, have no
-- source; and each whose file failed. Never written to the marker.
CREATE TABLE build_incomplete (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    reason TEXT NOT NULL CHECK (
        reason IN ('unresolved', 'no_source', 'failed', 'conflict', 'blocked', 'missing_dependency')
    ),
    grp TEXT NOT NULL,
    game TEXT NOT NULL,
    target TEXT NOT NULL,
    detail TEXT,
    PRIMARY KEY (build_id, seq)
);

-- The files in a Library Folder that a Build wrote, with their provenance: the ROMs as JSON ExpectedRoms, the sources
-- as JSON PlanSources. Written in the transaction that marks the item applied. A file a placement puts in the folder
-- is recorded built, of kind 'place', as well as placed.
CREATE TABLE built_files (
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    target TEXT NOT NULL,
    game TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('copy', 'repack', 'adopt', 'place')),
    build_id INTEGER NOT NULL REFERENCES builds (id),
    selection_id INTEGER NOT NULL REFERENCES selections (id),
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    dat_revision_id INTEGER NOT NULL REFERENCES dat_revisions (id),
    roms TEXT NOT NULL,
    sources TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    -- The SHA-256 of the whole file, which a check compares a changed file against.
    sha256 TEXT,
    -- Whether it is TorrentZip: false once a check accepted the right ROMs in another ZIP.
    torrentzip INTEGER NOT NULL DEFAULT 1,
    -- A file built loose, its one ROM under its DAT name, rather than as a TorrentZip.
    loose INTEGER NOT NULL DEFAULT 0,
    -- The Addition it was built from, when it was.
    addition_id INTEGER REFERENCES additions (id),
    -- A CHD, and the SHA-1 its v5 header recorded when its extraction was checked against the DAT, of its data and its
    -- metadata, such as a CD's track list: a later check reads the header alone and compares it, never decompressing.
    -- Neither depends on compression; the raw data SHA-1, of the data alone, is kept to say which changed.
    chd INTEGER NOT NULL DEFAULT 0,
    chd_sha1 TEXT,
    chd_raw_sha1 TEXT,
    -- A Disk of an Arcade DAT's Game, `<game>/<disk>.chd`, copied from its Source Set: its folder is one a Library
    -- Folder's page lists for RomM to leave out.
    disk INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (folder_id, target)
);
CREATE INDEX built_files_by_dat_revision ON built_files (dat_revision_id);

-- What a Build or recovery created in a Library Folder for itself: the staging directory, each file in it (named
-- '.stage-select-staging/<file>') and a marker's temp file, by device and inode, committed before it is relied on.
-- Only an entry found with a recorded identity is ever removed; anything else there is left and blocks the folder.
CREATE TABLE folder_temps (
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- The unique temporary name a directory Stage Select creates in a Library Folder is about to be made under, committed
-- before it is made, so that one a stop left there, before its identity was recorded in folder_temps, is still known
-- to be this store's: recovery records it, when it is the empty private directory made, and leaves it.
CREATE TABLE folder_temp_names (
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    name TEXT NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- The directories Stage Select created in a Library Folder that outlive a Build: the retention area
-- ('.stage-select-retained'), each generation's folder in it ('.stage-select-retained/<generation>') and the private
-- folder kept files are deleted from ('.stage-select-retained/.deleting'), by device and inode, committed before
-- anything is moved into them. Each is created under a temporary name recorded in folder_temps, then renamed into
-- place without replacing anything. One found with another identity, or not recorded at all, is never used or removed.
CREATE TABLE folder_dirs (
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- A read-only check of a Library Folder, by a Check Job or a plan, at the folder's generation then: its CheckSummary
-- and FolderCheck JSON. Older checks no plan refers to are dropped.
CREATE TABLE folder_checks (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    job_id INTEGER REFERENCES jobs (id),
    generation INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    summary TEXT NOT NULL,
    report TEXT NOT NULL
);
CREATE INDEX folder_checks_by_folder ON folder_checks (folder_id, id);

-- What a check read of an entry Stage Select did not build, or of a built file in conflict, by its size and
-- modification time: so that an entry unchanged since is not read again. Probe JSON.
CREATE TABLE folder_probes (
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    name TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    probe TEXT NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- An entry in a Library Folder Stage Select did not build that the Operator acknowledged: a check reports it as
-- acknowledged rather than as drift while it is the same entry, by name, identity, size and modification time, and
-- drops the acknowledgement once it is not. Acknowledging writes nothing to the folder.
CREATE TABLE drift_acks (
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    build_id INTEGER NOT NULL REFERENCES builds (id),
    acknowledged_at TEXT NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- A plan to delete kept files: KeptDeletionContent JSON and the SHA-256 of that JSON, which an approval binds to. An
-- approved one not finished is being carried out; while it is, no Build or Check of its folder starts.
CREATE TABLE kept_deletions (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    content TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    approval_digest TEXT,
    actor TEXT,
    approved_at TEXT,
    finished_at TEXT,
    -- KeptDeletionOutcome JSON, once carried out.
    outcomes TEXT,
    -- Why recovery left it unfinished when the service last started, if it did.
    recovery_error TEXT,
    -- A deletion of a conversion's original, approved with the plan that converted it: its file is deleted only by
    -- its trusted stamp, in recovery too.
    converted INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX kept_deletions_by_folder ON kept_deletions (folder_id, id);

-- A file a Build moved from its Library Folder into '.stage-select-retained/<generation>/<kept_name>', with what it
-- was: its name, its SHA-256, size and identity, and, for a file Stage Select built, its game, ROMs and provenance.
-- Recorded 'keeping', the intent, before it is moved, and 'kept' once it is found moved; recovery settles a 'keeping'
-- one from what is in the folder. 'deleting' is the intent to delete it, recorded before it is moved into the private
-- '.deleting' folder and removed there; 'quarantined' when what was moved there is not the file, until it is moved
-- back. 'gone' is one found already gone, with its folder's marker and retention area checked, when its deletion was
-- carried out. 'restoring' is the intent to move it back into the folder, recorded with its Build's item before it is
-- moved; 'restored' once it is found there.
--
-- It may be a directory, moved whole and never read, with no SHA-256; an entry Stage Select did not build ('drift'),
-- or a built file a cleanup repacked ('repack'). 'probe' is what reading it found, as a JSON Probe, reused while its
-- identity, size, modification and status change times are 'probe_ctime_ns' and those recorded.
CREATE TABLE kept_files (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    generation INTEGER NOT NULL,
    name TEXT NOT NULL,
    kept_name TEXT NOT NULL,
    reason TEXT NOT NULL CHECK (reason IN ('removal', 'replacement', 'conflict', 'drift', 'repack')),
    built_by INTEGER REFERENCES builds (id),
    game TEXT,
    roms TEXT,
    selection_id INTEGER REFERENCES selections (id),
    audit_id INTEGER REFERENCES audits (id),
    dat_revision_id INTEGER REFERENCES dat_revisions (id),
    sha256 TEXT,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('keeping', 'kept', 'lost', 'deleting', 'quarantined', 'deleted', 'gone',
                                         'restoring', 'restored')),
    error TEXT,
    created_at TEXT NOT NULL,
    kept_at TEXT,
    deletion_id INTEGER REFERENCES kept_deletions (id),
    deleted_at TEXT,
    -- The identity of what was found at '.stage-select-retained/.deleting/<id>' when it was not the kept file, while
    -- the state is 'quarantined': it is to be moved back to the kept entry, and the deletion is not finished until it
    -- is.
    quarantine_dev INTEGER,
    quarantine_ino INTEGER,
    directory INTEGER NOT NULL DEFAULT 0,
    probe TEXT,
    probe_ctime_ns INTEGER,
    restored_at TEXT,
    -- Its ctime read, just after the rename that kept it, through the descriptor held across that rename: with the
    -- SHA-256 its conversion read it as, the only stamp that may authorize deleting a converted original.
    kept_ctime_ns TEXT,
    -- Its ctime read through the descriptor held across its rename into the private folder it is deleted from, as a
    -- conversion deletes it: what it must still have there to be deleted.
    slot_ctime_ns TEXT,
    CHECK (directory = 1 OR sha256 IS NOT NULL)
);
CREATE INDEX kept_files_by_folder ON kept_files (folder_id, generation, id);
CREATE UNIQUE INDEX kept_files_by_name ON kept_files (folder_id, generation, kept_name)
WHERE state IN ('keeping', 'kept', 'deleting', 'quarantined', 'restoring');
CREATE INDEX kept_files_by_dat_revision ON kept_files (dat_revision_id);

-- What a Play Library conversion read of each original before it put the converted file in place: its SHA-256,
-- identity and stamp, read through one descriptor while they held still, and recorded before the publish. Never
-- updated, whatever a check later records of the file: an original is deleted only while it still reads as this.
CREATE TABLE conversion_originals (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES library_folders (id),
    build_id INTEGER NOT NULL REFERENCES builds (id),
    target TEXT NOT NULL,
    converts_to TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX conversion_originals_by_target ON conversion_originals (folder_id, target);
CREATE TRIGGER conversion_originals_never_change BEFORE UPDATE ON conversion_originals
BEGIN
    SELECT RAISE(ABORT, 'a conversion record is never changed');
END;

------------------------------------------------------------------------------------------------------------------------
-- Single Copy (ADR 0023)
------------------------------------------------------------------------------------------------------------------------

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
-- the size, modification time, device, inode, status change time and SHA-256 it had when it was placed, or a Repair
-- or check last recorded it: what a check of the folder compares it with. A file whose stamp differs in any of them
-- is read again. An Audit reads only these files of the folder. The SHA-256 is null when it was placed by a rename,
-- which reads nothing: its Audit's payload Evidence stands for it, and a check trusts it only while its stamp is as
-- recorded.
CREATE TABLE placed_files (
    folder_id INTEGER NOT NULL,
    path TEXT NOT NULL,
    source_set_id INTEGER NOT NULL,
    game TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    sha256 TEXT,
    -- A file that is a CHD: chd_sha1, the SHA-1 its header held when extracting it gave back every track of its disc
    -- exactly, which a quick inventory takes as Claim Evidence; chd_roms, its disc's ROMs as the DAT listed them then,
    -- JSON as a plan item's, its cue sheet among them for a CD; and cue_sha256, for a CD, the DAT-exact cue sheet a CHD
    -- doesn't hold, kept in the store under cues/<cue_sha256>. Null for every other placed file.
    chd_sha1 TEXT,
    chd_roms TEXT,
    cue_sha256 TEXT,
    PRIMARY KEY (folder_id, path),
    FOREIGN KEY (source_set_id, game, folder_id) REFERENCES placed_games (source_set_id, game, folder_id)
);
CREATE INDEX placed_files_by_game ON placed_files (source_set_id, game);

-- Each Source Set file a placing Build item consumes, by its path relative to the Source Set's folder, as the plan
-- found it: deleted once the Game is placed, under ADR 0010's trusted-stamp rule. sha256 is what it was read as while
-- it held still, recorded before anything is written for it; slot and moved_ctime_ns its place in the Source Set's
-- private .deleting folder, recorded before it is moved there, and the change time read through the descriptor held
-- across that move. One that can't be proven unchanged is moved into the Source Set's kept files as kept_name, of
-- generation kept_generation, recorded before it is moved, and its source_kept row is kept_id. state: 'pending',
-- 'deleted', 'kept', or 'gone' when it was gone, with its folder, before it was set aside.
CREATE TABLE placement_originals (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    n INTEGER NOT NULL,
    path TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    sha256 TEXT,
    slot TEXT,
    moved_ctime_ns TEXT,
    kept_name TEXT,
    kept_generation INTEGER,
    kept_id INTEGER REFERENCES source_kept (id),
    state TEXT NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'deleted', 'kept', 'gone')),
    detail TEXT,
    PRIMARY KEY (build_id, seq, n)
);

-- Each placed file a return item takes, as the plan found it, by its path relative to the Library Folder: n orders
-- them as the plan does. sha256 is what it was read as, whole, while it held still, before anything was written for it
-- (none for a rename, which reads nothing). keeps says the Library Folder keeps it as a built copy of its Game, which
-- is still selected there. One that goes is moved into the Library Folder's private .deleting folder as slot, recorded
-- first, and its change time read through the descriptor held across that move is moved_ctime_ns; it is deleted once
-- the release is committed, only while it still has that change time and SHA-256. One renamed home records there its
-- change time read the same way right after its rename: it is taken as home only while it still has it. state:
-- 'placed', 'deleted', 'gone' when it was gone before it could be deleted, 'left' when it could not be proven unchanged
-- and was put back, or 'putting_back' from just before one renamed home is put back until it is verified against the
-- DAT and recorded as found, or left recorded as it was: recovery verifies each so marked that it finds placed.
CREATE TABLE return_placed (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    n INTEGER NOT NULL,
    path TEXT NOT NULL,
    game TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    ctime_ns INTEGER NOT NULL,
    keeps INTEGER NOT NULL DEFAULT 0,
    sha256 TEXT,
    slot TEXT,
    moved_ctime_ns TEXT,
    state TEXT NOT NULL DEFAULT 'placed' CHECK (state IN ('placed', 'deleted', 'gone', 'left', 'putting_back')),
    detail TEXT,
    PRIMARY KEY (build_id, seq, n)
);

-- Each file a return item puts in the Source Set's folder, by its path relative to it: the name it is staged under in
-- the Source Set's private folder, recorded before it is made, its identity once it is, and its SHA-256 and size once
-- it is verified against the DAT; and chain, the identity of each folder from the Source Set's folder down to the one
-- it goes in, as JSON [[dev, ino], ...], recorded before anything is put in place. A placed file is deleted only once
-- every file of its release is in place below that chain, walked again from the Source Set's registered path with its
-- marker. A rename's file is its placed file n, renamed.
CREATE TABLE return_outputs (
    build_id INTEGER NOT NULL REFERENCES builds (id),
    seq INTEGER NOT NULL,
    n INTEGER NOT NULL,
    path TEXT NOT NULL,
    staged TEXT,
    dev INTEGER,
    ino INTEGER,
    sha256 TEXT,
    size INTEGER,
    chain TEXT,
    PRIMARY KEY (build_id, seq, n)
);

-- A Game that needs review because a Build proved its placed CHD doesn't hold its disc as the DAT lists it: extracting
-- it gave back other bytes, or chdman said it is damaged. A current condition of the Game, kept apart from every Audit,
-- whose verdicts record only what each read: raised by the Build build_id, about its placed file path as Audits name
-- it, with why, and cleared once an Audit verifies the Game by payload or a return verifies it home.
CREATE TABLE review_flags (
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    game TEXT NOT NULL,
    path TEXT NOT NULL,
    reason TEXT NOT NULL,
    build_id INTEGER NOT NULL REFERENCES builds (id),
    raised_at TEXT NOT NULL,
    PRIMARY KEY (source_set_id, game)
);

------------------------------------------------------------------------------------------------------------------------
-- Source Set repairs
------------------------------------------------------------------------------------------------------------------------

-- A repair Change Plan: RepairPlanContent JSON and the SHA-256 of that JSON, which an approval binds to. It is carried
-- out when approved; outcomes is a JSON array of RepairOutcome. audit_job_id is the incremental Audit it queued;
-- audit_owed is set while that Audit waits for another Job of the Source Set to end; readers and hashers are the
-- workers it runs with.
CREATE TABLE repair_plans (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    audit_id INTEGER NOT NULL REFERENCES audits (id),
    content TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    approved_digest TEXT,
    approved_by TEXT,
    approved_at TEXT,
    finished_at TEXT,
    outcomes TEXT,
    recovery_error TEXT,
    audit_owed INTEGER NOT NULL DEFAULT 0,
    audit_job_id INTEGER REFERENCES jobs (id),
    audit_error TEXT,
    readers INTEGER NOT NULL,
    hashers INTEGER NOT NULL,
    -- Set, with every step the cancellation skips, in one transaction: a restart never runs a skipped step.
    cancelled INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX repair_plans_unfinished ON repair_plans (source_set_id) WHERE approved_at IS NOT NULL
    AND finished_at IS NULL;

-- A durable intent: one step of an approved repair, committed with the approval before anything changes and advanced
-- before each change, so that recovery rolls it forward from where it stopped. detail is the RepairStep JSON.
CREATE TABLE repair_steps (
    id INTEGER PRIMARY KEY,
    plan_id INTEGER NOT NULL REFERENCES repair_plans (id),
    seq INTEGER NOT NULL,
    item TEXT NOT NULL,
    detail TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'staging', 'staged', 'moving', 'done', 'failed', 'skipped')),
    staged_name TEXT,
    staged_dev INTEGER,
    staged_ino INTEGER,
    sha256 TEXT,
    kept_id INTEGER REFERENCES source_kept (id),
    move_id INTEGER REFERENCES ingest_moves (id),
    error TEXT,
    note TEXT,
    finished_at TEXT,
    -- A Publish whose place was taken: the kept name its staged file, recorded in staged_*, is moved to, and why,
    -- recorded before it is moved.
    aside_name TEXT,
    aside_why TEXT,
    -- The staged file's size, modification time and SHA-256, read through the descriptor that wrote or checked it
    -- once it was written: only a file that still matches them is ever deleted, since a file put in its place may
    -- reuse its inode number.
    staged_size INTEGER,
    staged_mtime_ns TEXT,
    staged_sha256 TEXT,
    -- The slot in the private .deleting folder a staged file recorded in staged_* that won't be published is moved
    -- to, recorded before it is moved; cleared once the file is in neither place.
    discard_slot TEXT,
    -- The kept name a file found in that slot that can't be told to be the staged file is moved to, recorded before
    -- it is moved; it is kept, never deleted.
    discard_kept TEXT,
    -- The ctime of the staged file read, once it was moved into that slot, through the descriptor held across the
    -- rename; it is deleted only while it still has it.
    discard_ctime_ns TEXT,
    -- The SHA-256 of the one file a conversion's Build step read, hashed as it was read while its stamp held still.
    source_sha256 TEXT,
    -- A JSON object of each file a Build step read, by its path relative to the Source Set's folder, to the SHA-256 it
    -- read it as while its stamp held still: a conversion of a loose folder into one TorrentZip reads several
    -- originals, and each is deleted only by what was read of it. source_sha256 records a Build step that read one
    -- file.
    source_reads TEXT,
    -- A step that carries a file to a folder on another filesystem (crate::carry): it copies it under a staged name in
    -- the destination's folder, recorded with its identity before and once it is made, records the SHA-256 the
    -- original was read as, publishes the copy, then moves the original into its folder's private .deleting folder
    -- under a slot recorded first, and deletes it there only while it still has the change time read, through the
    -- descriptor held across that rename, and that SHA-256.
    copy_staged TEXT,
    copy_dev INTEGER,
    copy_ino INTEGER,
    copy_sha256 TEXT,
    copy_slot TEXT,
    copy_ctime_ns TEXT,
    UNIQUE (plan_id, seq)
);
CREATE INDEX repair_steps_unfinished ON repair_steps (plan_id) WHERE state NOT IN ('done', 'failed', 'skipped');
CREATE INDEX repair_steps_by_move ON repair_steps (move_id) WHERE move_id IS NOT NULL;

-- A file kept aside in a Source Set, at `.stage-select-retained/<generation>/<kept_name>`; name is where it was,
-- relative to the Source Set's folder. It is kept by a repair (plan_id) or, an original a placing Build couldn't prove
-- unchanged, by that Build (build_id): exactly one. The file is the Source Set's, and outlives its Library Folder:
-- build_id is the id of the Build that kept it, not a reference, so forgetting the folder, which forgets its Builds,
-- leaves it as it is. Forgetting the Source Set deletes the row with the repair plan that kept it, never clearing
-- plan_id first (lifecycle::OWNED).
CREATE TABLE source_kept (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    plan_id INTEGER REFERENCES repair_plans (id),
    build_id INTEGER,
    generation INTEGER NOT NULL,
    name TEXT NOT NULL,
    kept_name TEXT NOT NULL,
    reason TEXT NOT NULL,
    sha256 TEXT,
    size INTEGER NOT NULL,
    mtime_ns TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'kept' CHECK (state IN ('kept', 'deleting', 'deleted', 'gone')),
    deletion_id INTEGER REFERENCES source_kept_deletions (id),
    kept_at TEXT NOT NULL,
    deleted_at TEXT,
    error TEXT,
    -- Its ctime read, once it was moved into the private .deleting folder to be deleted, through the descriptor
    -- held across that rename; it is deleted only while it still has it.
    slot_ctime_ns TEXT,
    -- Its ctime read, just after the rename that kept it, through the descriptor held across that rename: with the
    -- SHA-256 its conversion read it as, the only stamp that may authorize deleting a converted original.
    kept_ctime_ns TEXT,
    UNIQUE (source_set_id, generation, kept_name),
    CHECK ((plan_id IS NULL) <> (build_id IS NULL))
);

-- Deleting kept files of a Source Set, as kept_deletions does for a Library Folder: SourceKeptDeletionContent JSON
-- and its digest; outcomes is a JSON array of KeptDeletionOutcome.
CREATE TABLE source_kept_deletions (
    id INTEGER PRIMARY KEY,
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    content TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    approved_digest TEXT,
    approved_by TEXT,
    approved_at TEXT,
    finished_at TEXT,
    outcomes TEXT,
    recovery_error TEXT
);

------------------------------------------------------------------------------------------------------------------------
-- Ingest Folders
------------------------------------------------------------------------------------------------------------------------

-- An Ingest Folder: a Root of kind `ingest`, registered by writing its marker. generation counts the plans applied to
-- the folder, each of which keeps its items under .stage-select-retained/<generation>/. Unregistered as a Source Set
-- is.
CREATE TABLE ingest_folders (
    id INTEGER PRIMARY KEY,
    root TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    last_scan_at TEXT,
    unregistered_at TEXT
);
CREATE UNIQUE INDEX ingest_folders_root ON ingest_folders (root) WHERE unregistered_at IS NULL;

-- The directories Stage Select created in an Ingest Folder, as folder_dirs, folder_temps and folder_temp_names are
-- for a Library Folder: the retention area, each generation's folder, and the private .deleting folder.
CREATE TABLE ingest_dirs (
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE ingest_temps (
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE ingest_temp_names (
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    name TEXT NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- An entry found in an Ingest Folder. dev, ino, size, mtime_ns and ctime_ns are as last seen; stable_since is when
-- they were first seen as they are. hashed_* are the stat the assessment's hashes were read under, reused while the
-- entry is still exactly that. Only one item that is not gone or moved holds a name.
CREATE TABLE ingest_items (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    name TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('arriving', 'assessed', 'unreadable', 'failed', 'stale', 'gone',
                                         'already_held', 'unsupported', 'dismissed', 'moved')),
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns TEXT NOT NULL,
    ctime_ns TEXT NOT NULL,
    stable_since TEXT NOT NULL,
    -- Whether its stat has stayed as recorded for the folder's settle interval, whatever state it is shown in.
    settled INTEGER NOT NULL DEFAULT 0,
    hashed_dev INTEGER,
    hashed_ino INTEGER,
    hashed_size INTEGER,
    hashed_mtime_ns TEXT,
    hashed_ctime_ns TEXT,
    probe TEXT,
    assessment TEXT,
    reason TEXT,
    first_seen_at TEXT NOT NULL,
    assessed_at TEXT,
    changed_at TEXT NOT NULL
);
CREATE UNIQUE INDEX ingest_items_by_name ON ingest_items (folder_id, name) WHERE state NOT IN ('gone', 'moved');

-- A plan for items of an Ingest Folder: IngestPlanContent JSON and the SHA-256 of that JSON, which an approval binds
-- to. It is carried out when approved; outcomes is a JSON array of IngestOutcome.
CREATE TABLE ingest_plans (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    content TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    approved_digest TEXT,
    approved_by TEXT,
    approved_at TEXT,
    finished_at TEXT,
    outcomes TEXT,
    recovery_error TEXT
);
CREATE INDEX ingest_plans_by_folder ON ingest_plans (folder_id, id);

-- A file kept aside in an Ingest Folder's .stage-select-retained/<generation>/, by the plan that moved it there, if a
-- plan did.
CREATE TABLE ingest_kept (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    plan_id INTEGER REFERENCES ingest_plans (id),
    item_id INTEGER REFERENCES ingest_items (id),
    generation INTEGER NOT NULL,
    name TEXT NOT NULL,
    kept_name TEXT NOT NULL,
    reason TEXT NOT NULL,
    sha256 TEXT,
    size INTEGER NOT NULL,
    mtime_ns TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    kept_at TEXT NOT NULL,
    UNIQUE (folder_id, generation, kept_name)
);

-- A durable intent to move one entry out of an Ingest Folder, committed before anything is moved and advanced before
-- each step, so that a stop rolls forward from where it was. kind is how it moves: one rename that never replaces
-- anything, a verified copy whose original is then deleted, or a verified TorrentZip repack whose original is then
-- kept. dest_kind 'kept' is the folder's own kept files, under generation; 'path' is the folder rel in the writable
-- Root dest_root. staged_* is the file a copy or repack writes before it is renamed to dest_name. A copy's original is
-- deleted only when the copy reads as exactly the ROM exact_rom; any other original is kept, in state 'retaining', as
-- a repack's is.
CREATE TABLE ingest_moves (
    id INTEGER PRIMARY KEY,
    folder_id INTEGER NOT NULL REFERENCES ingest_folders (id),
    plan_id INTEGER REFERENCES ingest_plans (id),
    item_id INTEGER REFERENCES ingest_items (id),
    kind TEXT NOT NULL CHECK (kind IN ('rename', 'copy', 'repack')),
    state TEXT NOT NULL CHECK (state IN ('pending', 'moving', 'restoring', 'staging', 'published', 'disposing',
                                         'keeping', 'retaining', 'done', 'failed')),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns TEXT NOT NULL,
    ctime_ns TEXT NOT NULL,
    sha256 TEXT,
    dest_kind TEXT NOT NULL CHECK (dest_kind IN ('kept', 'path')),
    dest_root TEXT,
    dest_rel TEXT,
    generation INTEGER,
    dest_name TEXT NOT NULL,
    reason TEXT,
    roms TEXT,
    exact_rom TEXT,
    staged_name TEXT,
    staged_dev INTEGER,
    staged_ino INTEGER,
    dest_sha256 TEXT,
    -- An entry found where the planned one should have been, and the kept files name it is set aside under.
    found_dev INTEGER,
    found_ino INTEGER,
    aside_name TEXT,
    kept_id INTEGER REFERENCES ingest_kept (id),
    error TEXT,
    -- What a move that ended did besides what was planned, such as keeping an original that changed.
    note TEXT,
    created_at TEXT NOT NULL,
    finished_at TEXT,
    -- The staged copy as it was once written and checked, read through the descriptor that wrote or checked it, and
    -- the change time of its published copy, read through that descriptor, held across the rename, once published:
    -- only a copy that still matches both, with its inode number, may authorize deleting its original, since a file
    -- put in its place may reuse the number. A move without them keeps its original.
    staged_size INTEGER,
    staged_mtime_ns TEXT,
    published_ctime_ns TEXT,
    -- The ctime of an original read, once it was set aside in the private .deleting folder, through the descriptor
    -- held across that rename; it is deleted only while it still has it.
    isolated_ctime_ns TEXT,
    -- A move that writes one ROM as a loose file, extracted from its ZIP, rather than a TorrentZip.
    loose INTEGER NOT NULL DEFAULT 0,
    -- A move that leaves its original where it is once its copy is published: one of several extracting the ROMs of a
    -- ZIP into a loose folder, the last of which disposes of the ZIP.
    leave_original INTEGER NOT NULL DEFAULT 0,
    -- A move into a folder at a path records, before anything is moved there, the device and inode of that folder and
    -- of each folder below it down to the one its file goes in (dest_chain, a JSON array of [dev, ino]): its original
    -- is consumed only while the folders at that path are still those, and a stop that finds them moved or replaced
    -- leaves it unfinished, with why in blocked, until they are back.
    dest_chain TEXT,
    blocked TEXT,
    -- A move whose published copy failed its last check returns its original to the Ingest Folder: why is recorded
    -- here before the original leaves the private .deleting folder, so that a stop finishes the return rather than
    -- read an empty slot as a deletion.
    return_reason TEXT
);
CREATE INDEX ingest_moves_unfinished ON ingest_moves (state) WHERE state NOT IN ('done', 'failed');
-- Each ingest item shows its latest move and the Job carrying it out: find an item's moves, and what a move became,
-- without reading every move.
CREATE INDEX ingest_moves_by_item ON ingest_moves (item_id, id) WHERE item_id IS NOT NULL;

-- An Audit a DAT import owes each Source Set bound to the imported DAT, recorded before the import so that one missed
-- while another Audit of the set ran, or by a stop, is queued later. Until it is queued it holds the revision's blob.
-- One that cannot be queued ends with its error; one that only failed this time keeps its last error and stays owed.
CREATE TABLE ingest_reaudits (
    id INTEGER PRIMARY KEY,
    plan_id INTEGER NOT NULL REFERENCES ingest_plans (id),
    item_id INTEGER NOT NULL REFERENCES ingest_items (id),
    source_set_id INTEGER NOT NULL REFERENCES source_sets (id),
    sha256 TEXT NOT NULL,
    actor TEXT NOT NULL,
    readers INTEGER NOT NULL,
    hashers INTEGER NOT NULL,
    job_id INTEGER,
    created_at TEXT NOT NULL,
    fulfilled_at TEXT,
    error TEXT,
    UNIQUE (plan_id, item_id, source_set_id)
);
CREATE INDEX ingest_reaudits_owed ON ingest_reaudits (id) WHERE fulfilled_at IS NULL;
CREATE INDEX ingest_reaudits_owed_by_sha ON ingest_reaudits (sha256) WHERE fulfilled_at IS NULL;

------------------------------------------------------------------------------------------------------------------------
-- Additions
------------------------------------------------------------------------------------------------------------------------

-- An Additions Set: files no DAT knows that the Operator chose to keep, each identified by its own hashes, for one
-- system (DAT) at <path> in the Root named 'additions'. Its folder is created by the first addition of its system
-- under the recorded temporary name temp_name, checked to be the private, empty directory made (temp_dev, temp_ino),
-- given its marker, and renamed to path without replacing anything; dev and ino are recorded once it is in place, with
-- marker_at. setting_aside_at is when the Operator approved setting it aside; it is Unregistered, as a Source Set is,
-- once every addition is retired.
CREATE TABLE additions_sets (
    id INTEGER PRIMARY KEY,
    dat_id INTEGER NOT NULL REFERENCES dats (id),
    root TEXT NOT NULL,
    path TEXT NOT NULL,
    dev INTEGER,
    ino INTEGER,
    temp_name TEXT,
    temp_dev INTEGER,
    temp_ino INTEGER,
    marker_at TEXT,
    created_at TEXT NOT NULL,
    setting_aside_at TEXT,
    unregistered_at TEXT
);
CREATE UNIQUE INDEX additions_sets_dat ON additions_sets (dat_id) WHERE unregistered_at IS NULL;
CREATE UNIQUE INDEX additions_sets_folder ON additions_sets (root, path) WHERE unregistered_at IS NULL;

-- A temporary folder an attempt to create an Additions Set's folder left behind, by name and, when it was recorded,
-- identity: recorded before another attempt replaces temp_name, so that it is never forgotten. It is left where it is,
-- for the Operator to remove, and never deleted by Stage Select.
CREATE TABLE additions_set_leftovers (
    id INTEGER PRIMARY KEY,
    set_id INTEGER NOT NULL REFERENCES additions_sets (id),
    name TEXT NOT NULL,
    dev INTEGER,
    ino INTEGER,
    found_at TEXT NOT NULL,
    UNIQUE (set_id, name)
);

-- The directories Stage Select created in an Additions Set's folder to keep retired additions' files, and those being
-- created, as folder_dirs, folder_temps and folder_temp_names record them for a Library Folder.
CREATE TABLE additions_dirs (
    folder_id INTEGER NOT NULL REFERENCES additions_sets (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE additions_temps (
    folder_id INTEGER NOT NULL REFERENCES additions_sets (id),
    name TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    PRIMARY KEY (folder_id, name)
);
CREATE TABLE additions_temp_names (
    folder_id INTEGER NOT NULL REFERENCES additions_sets (id),
    name TEXT NOT NULL,
    PRIMARY KEY (folder_id, name)
);

-- An Addition. rom is the ExpectedRom JSON of its one ROM, with its own hashes, read through a descriptor held on the
-- file while it stayed still, and recorded when it was approved: by a scan for an ingest item, by the repair plan for
-- a Source Set Entry and by the check for drift. Its file is file_name in the set, written as TorrentZip or loose as
-- loose says, and is published only once it holds exactly that ROM.
--
-- How it gets there is a durable intent, recorded before anything changes:
-- - from an Ingest Folder, by the ingest move move_id, which writes file_name itself;
-- - from a Source Set, once the Keep step of item repair_item of repair repair_plan_id has kept it aside;
-- - from Drift, once the cleanup plan cleanup_plan_id has kept the entry origin aside;
-- the latter two are then published from that kept file, which is never deleted: staged_name is recorded before the
-- staged file is created, staged_dev and staged_ino once it is, and the staged file is renamed to file_name without
-- replacing anything.
--
-- dev, ino, size, mtime_ns, ctime_ns and sha256 are its published file as Stage Select wrote and read it back through
-- the descriptor it held: what a build reads, and what a check compares the folder with.
CREATE TABLE additions (
    id INTEGER PRIMARY KEY,
    set_id INTEGER NOT NULL REFERENCES additions_sets (id),
    name TEXT NOT NULL,
    file_name TEXT NOT NULL,
    rom TEXT NOT NULL,
    parent_game TEXT,
    source TEXT NOT NULL CHECK (source IN ('ingest', 'drift', 'source_set')),
    origin TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK (state IN ('pending', 'staging', 'active', 'retiring', 'retired', 'restoring', 'failed')),
    error TEXT,
    move_id INTEGER REFERENCES ingest_moves (id),
    repair_plan_id INTEGER REFERENCES repair_plans (id),
    repair_item TEXT,
    cleanup_plan_id INTEGER REFERENCES plans (id),
    staged_name TEXT,
    staged_dev INTEGER,
    staged_ino INTEGER,
    dev INTEGER,
    ino INTEGER,
    size INTEGER,
    mtime_ns TEXT,
    ctime_ns TEXT,
    sha256 TEXT,
    approved_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    published_at TEXT,
    retired_at TEXT,
    -- How many times the Operator asked to retire or restore it: one asked after its Additions Job visited it is
    -- handed on.
    requests INTEGER NOT NULL DEFAULT 0,
    -- Whether file_name is a loose file, its ROM itself, rather than a TorrentZip: what the storage policy said for
    -- its system's source sets when it was approved.
    loose INTEGER NOT NULL DEFAULT 0,
    CHECK ((source = 'ingest') = (move_id IS NOT NULL)),
    CHECK ((source = 'source_set') = (repair_plan_id IS NOT NULL AND repair_item IS NOT NULL)),
    CHECK ((source = 'drift') = (cleanup_plan_id IS NOT NULL)),
    CHECK (state NOT IN ('active', 'retiring', 'restoring') OR (dev IS NOT NULL AND sha256 IS NOT NULL))
);
CREATE UNIQUE INDEX additions_file_in_use ON additions (set_id, file_name)
WHERE state IN ('pending', 'staging', 'active', 'retiring', 'restoring');
CREATE UNIQUE INDEX additions_name_in_use ON additions (set_id, name)
WHERE state IN ('pending', 'staging', 'active', 'retiring', 'restoring');
CREATE INDEX additions_unfinished ON additions (state)
WHERE state IN ('pending', 'staging', 'retiring', 'restoring');
CREATE INDEX additions_by_move ON additions (move_id) WHERE move_id IS NOT NULL;

-- Deleting kept files of an Additions Set, as source_kept_deletions records it for a Source Set.
CREATE TABLE additions_kept_deletions (
    id INTEGER PRIMARY KEY,
    additions_set_id INTEGER NOT NULL REFERENCES additions_sets (id),
    content TEXT NOT NULL,
    digest TEXT NOT NULL,
    created_at TEXT NOT NULL,
    approved_digest TEXT,
    approved_by TEXT,
    approved_at TEXT,
    finished_at TEXT,
    outcomes TEXT,
    recovery_error TEXT
);

-- A retired addition's file, kept in its Additions Set at `.stage-select-retained/<generation>/<kept_name>`, where
-- generation is the addition's id; name is the file name it was published under. A retirement is recorded here as
-- 'keeping', with the published file's identity, size, mtime and SHA-256 that Stage Select wrote and read back through
-- the descriptor it held, before the file is moved; it is 'kept' once it is found there, and 'restored' once a
-- restoration moved it back. It is deleted only as source_kept is, once the Operator approves it, and only while it is
-- still the file recorded, read through a descriptor while it holds still; a stamp read from a path never authorizes
-- it.
CREATE TABLE additions_kept (
    id INTEGER PRIMARY KEY,
    additions_set_id INTEGER NOT NULL REFERENCES additions_sets (id),
    addition_id INTEGER NOT NULL REFERENCES additions (id),
    generation INTEGER NOT NULL,
    name TEXT NOT NULL,
    kept_name TEXT NOT NULL,
    reason TEXT NOT NULL,
    sha256 TEXT,
    size INTEGER NOT NULL,
    mtime_ns TEXT NOT NULL,
    dev INTEGER NOT NULL,
    ino INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'keeping'
        CHECK (state IN ('keeping', 'kept', 'deleting', 'deleted', 'gone', 'restored')),
    deletion_id INTEGER REFERENCES additions_kept_deletions (id),
    kept_at TEXT NOT NULL,
    deleted_at TEXT,
    error TEXT,
    slot_ctime_ns TEXT
);
CREATE UNIQUE INDEX additions_kept_in_place ON additions_kept (additions_set_id, generation, kept_name)
WHERE state IN ('keeping', 'kept', 'deleting');

-- The additions each Additions Job settles: those to settle when it was queued, and those that joined it before it
-- started. An addition its Job left to settle joins a later one.
CREATE TABLE additions_job_members (
    job_id INTEGER NOT NULL REFERENCES jobs (id),
    addition_id INTEGER NOT NULL REFERENCES additions (id),
    PRIMARY KEY (job_id, addition_id)
);
CREATE INDEX additions_job_members_by_addition ON additions_job_members (addition_id, job_id);

------------------------------------------------------------------------------------------------------------------------
-- RomM (ADR 0020)
------------------------------------------------------------------------------------------------------------------------

-- The RomM scan a Build Job requested once it ended: at most one per Job. `platform` is the RomM platform the Library
-- Folder mapped to then, as JSON; `outcome` anything but 'requested' or 'finished' is a warning on the Job, which
-- still succeeds. `additions` is how the Additions collection was kept in step once the scan finished, as JSON; a
-- warning, never the Job's failure.
CREATE TABLE romm_scans (
    job_id INTEGER PRIMARY KEY REFERENCES jobs (id),
    platform TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN
        ('requested', 'finished', 'stopped', 'failed', 'timed_out', 'unavailable', 'error', 'unmapped')),
    task_id TEXT,
    detail TEXT,
    requested_at TEXT NOT NULL,
    ended_at TEXT,
    additions TEXT
);

-- RomM's "Additions" collection: at most one, tracked by RomM's id for it at the address it was made at, never by its
-- name.
CREATE TABLE romm_additions_collection (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    base_url TEXT NOT NULL,
    collection_id INTEGER NOT NULL
);

-- The collection to delete once RomM was turned off or its address changed, with the address and token it was made
-- through, until RomM confirms it is gone. At most one: a newer one replaces it. The token is a secret, kept as the
-- saved one is, and never returned or logged.
CREATE TABLE romm_released_collection (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    base_url TEXT NOT NULL,
    token TEXT NOT NULL,
    collection_id INTEGER NOT NULL
);

-- The latest comparison of RomM's view of a Library Folder with the files its Builds made: one per folder, replaced
-- each time. `error` says why one couldn't be done, and then it has no counts or rows. `rows` is JSON: each built file
-- RomM sees otherwise, with its flags. Its ROM ids are RomM's at `base_url` for the platform `fs_slug`, so a rescan is
-- sent only while both are still RomM's address and the folder's platform.
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
