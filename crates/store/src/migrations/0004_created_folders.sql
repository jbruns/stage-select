-- The folder a Root's registration created, absolute, on that change; NULL when it created none.
ALTER TABLE root_changes ADD COLUMN created_folder TEXT;

-- Who had Stage Select create a Source Set's folder when registering it, and when; NULL when its folder was there.
ALTER TABLE source_sets ADD COLUMN folder_created_by TEXT;
ALTER TABLE source_sets ADD COLUMN folder_created_at TEXT;
