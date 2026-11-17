-- Coverage (#180, ADR 0021): the Source Set's Coverage a Selection was made under, as a JSON array of the Selection
-- engine's region names. Null covered every Game. A Library Folder is Stale when its Source Set's Coverage differs
-- from its current Build's Selection's.
ALTER TABLE selections ADD COLUMN coverage TEXT;
