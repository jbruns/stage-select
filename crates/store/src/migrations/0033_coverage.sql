-- Coverage (#177, ADR 0021): the regions of its DAT's Games a Source Set intends to preserve, as a JSON array of the
-- Selection engine's region names. Null covers every Game.
ALTER TABLE source_sets ADD COLUMN coverage TEXT;
