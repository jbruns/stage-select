-- Set Layout (#247, ADR 0026): where a MAME system's Source Sets keep a family's shared ROMs, non_merged, split or
-- merged; source sets only. Null for a system that follows the default, and for the default, non_merged.
ALTER TABLE storage_policy ADD COLUMN set_layout TEXT;
