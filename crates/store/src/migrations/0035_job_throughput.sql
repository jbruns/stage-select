-- Throughput (#190): what a Job read and wrote, in bytes, and for how long it was metered, recorded as it ends so a
-- finished Job shows its average throughput. Null for a Job that ended before it was metered.
ALTER TABLE jobs ADD COLUMN bytes_read INTEGER;
ALTER TABLE jobs ADD COLUMN bytes_written INTEGER;
ALTER TABLE jobs ADD COLUMN duration_ms INTEGER;
