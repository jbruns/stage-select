-- An Entry that counts towards one Game alone, which another file named for it makes Complete, records that Game and
-- file (#369, ADR 0017): a duplicate. Both are null otherwise, as for every Entry recorded before.
ALTER TABLE audit_entries ADD COLUMN complete_in_game TEXT;
ALTER TABLE audit_entries ADD COLUMN complete_in_path TEXT;
