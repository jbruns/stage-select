-- A Game's Dependencies (#246, ADR 0025): a JSON array of the BIOSes and devices it reaches through `romof` and
-- `device_ref`, each with the Game Status the same Audit found for it. An earlier Audit has none until the Source Set is
-- audited again.
ALTER TABLE audit_games ADD COLUMN dependencies TEXT NOT NULL DEFAULT '[]';
