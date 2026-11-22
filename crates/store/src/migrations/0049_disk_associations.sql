-- What an Audit resolved about Disks (#254, ADR 0026): each Game of an Audit names its Disks, and each CHD's Entry the
-- Disks it counts for, as JSON in DAT order; an Unreadable delta CHD records its parent's combined SHA-1. An Audit
-- stored before this names none until the Source Set is audited again.
ALTER TABLE audit_games ADD COLUMN disks TEXT NOT NULL DEFAULT '[]';
ALTER TABLE audit_entries ADD COLUMN disks TEXT NOT NULL DEFAULT '[]';
ALTER TABLE audit_entries ADD COLUMN parent_sha1 TEXT;
