-- How many files a stat walk found unconfirmed (#374, ADR 0031): their device, inode, size and modification time as
-- the Audit it compared with recorded them, only their change time moved, as a change of permissions or owner moves
-- it. They don't make the Source Set Stale, and its next Incremental Audit reads them again. Walks before this found
-- them changed, and keep that count.
ALTER TABLE stat_walks ADD COLUMN unconfirmed INTEGER NOT NULL DEFAULT 0;
