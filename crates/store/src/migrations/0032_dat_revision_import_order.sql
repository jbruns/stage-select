-- A DAT Revision's import order, apart from its row id: a deleted revision imported again keeps its row (0031), and
-- must still be newer than every revision imported before it when their headers order them equally.
ALTER TABLE dat_revisions ADD COLUMN import_seq INTEGER NOT NULL DEFAULT 0;
UPDATE dat_revisions SET import_seq = id;

-- What finds the work and history depending on a DAT Revision, so listing what blocks each revision's deletion stays
-- quick as history grows.
CREATE INDEX builds_by_plan ON builds (plan_id);
CREATE INDEX builds_by_dat_revision ON builds (dat_revision_id);
CREATE INDEX built_files_by_dat_revision ON built_files (dat_revision_id);
CREATE INDEX kept_files_by_dat_revision ON kept_files (dat_revision_id);
CREATE INDEX jobs_by_dat_revision ON jobs (dat_revision_id) WHERE dat_revision_id IS NOT NULL;
CREATE INDEX ingest_reaudits_owed_by_sha ON ingest_reaudits (sha256) WHERE fulfilled_at IS NULL;
