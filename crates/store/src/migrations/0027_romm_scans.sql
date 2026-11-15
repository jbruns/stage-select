-- The RomM scan a Build Job requested once it ended (#141, ADR 0020): at most one per Job. `platform` is the
-- RomM platform the Library Folder mapped to then, as JSON; `outcome` anything but 'requested' or 'finished' is a
-- warning on the Job, which still succeeds.
CREATE TABLE romm_scans (
    job_id INTEGER PRIMARY KEY REFERENCES jobs (id),
    platform TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN
        ('requested', 'finished', 'failed', 'timed_out', 'unavailable', 'error', 'unmapped')),
    task_id TEXT,
    detail TEXT,
    requested_at TEXT NOT NULL,
    ended_at TEXT
);
