-- Calibration (#191, ADR 0022): a Job that measures how many hash workers the Host sustains and how many payload
-- readers each Root does, and records each sweep's curve and knee.

-- One row for each Calibration Job that measured the Host: the CPUs the Service could use then, the curve as JSON (a
-- list of {workers, bytes, duration_ms, bytes_per_s}), and its knee and the throughput there. A Calibration cancelled
-- before its Host sweep finished has none.
CREATE TABLE calibrations (
    job_id INTEGER PRIMARY KEY REFERENCES jobs (id),
    cpus INTEGER NOT NULL,
    curve TEXT NOT NULL,
    knee INTEGER,
    knee_bytes_per_s REAL,
    measured_at TEXT NOT NULL
);

-- One row for each Root a Calibration Job finished measuring, under its name and the path it had then. A Root that
-- had too little to read is 'too_small', one that couldn't be read is 'unreadable', and one in which the sweep couldn't
-- find enough to read in time is 'timed_out', each with no curve points and no knee. available_bytes is what its
-- sweep read.
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

-- The jobs table again, with 'calibration' among the kinds. A Calibration names no Source Set, DAT Revision or
-- folder, and only one is Queued or Running at a time.
CREATE TABLE jobs_new (
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
INSERT INTO jobs_new (id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error,
                      readers, hashers, created_at, started_at, finished_at, version, kind, profile_version_id,
                      selection_id, library_folder_id, plan_id, evidence, ingest_folder_id, repair_plan_id,
                      cancel_requested_at, additions_set_id, bytes_read, bytes_written, duration_ms)
SELECT id, state, source_set_id, dat_revision_id, actor, attempt, requeued_from, audit_id, error,
       readers, hashers, created_at, started_at, finished_at, version, kind, profile_version_id,
       selection_id, library_folder_id, plan_id, evidence, ingest_folder_id, repair_plan_id,
       cancel_requested_at, additions_set_id, bytes_read, bytes_written, duration_ms
FROM jobs;
DROP TABLE jobs;
ALTER TABLE jobs_new RENAME TO jobs;
CREATE INDEX jobs_by_state ON jobs (state, id);
CREATE INDEX jobs_by_repair_plan ON jobs (repair_plan_id) WHERE repair_plan_id IS NOT NULL;
CREATE INDEX jobs_by_dat_revision ON jobs (dat_revision_id) WHERE dat_revision_id IS NOT NULL;
CREATE UNIQUE INDEX jobs_one_active_per_source_set ON jobs (source_set_id, kind)
WHERE state IN ('queued', 'running') AND kind IN ('audit', 'selection', 'repair', 'stat_walk');
CREATE UNIQUE INDEX jobs_one_active_per_folder ON jobs (library_folder_id)
WHERE state IN ('queued', 'running') AND kind IN ('build', 'check');
CREATE UNIQUE INDEX jobs_one_active_per_ingest_folder ON jobs (ingest_folder_id)
WHERE state IN ('queued', 'running') AND kind = 'ingest_scan';
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
