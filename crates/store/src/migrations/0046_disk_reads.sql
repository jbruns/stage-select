-- A Disk's payload read (#228, ADR 0026): the combined SHA-1 `chdman verify` proved a CHD's data hashes to, which an
-- Incremental Audit reuses while the file's stamp holds. NULL for every other file's read.
ALTER TABLE payload_reads ADD COLUMN disk_sha1 TEXT;
