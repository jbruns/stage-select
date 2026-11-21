-- A ROM's dump status and whether it is optional (#232, ADR 0025): each Game of an Audit names its nodump, baddump and
-- optional ROMs, as JSON lists in DAT order. A nodump or optional ROM never lowers a Game Status. An Audit stored
-- before this lists none until the Source Set is audited again.
ALTER TABLE audit_games ADD COLUMN not_dumped TEXT NOT NULL DEFAULT '[]';
ALTER TABLE audit_games ADD COLUMN bad_dumps TEXT NOT NULL DEFAULT '[]';
ALTER TABLE audit_games ADD COLUMN optional_roms TEXT NOT NULL DEFAULT '[]';
