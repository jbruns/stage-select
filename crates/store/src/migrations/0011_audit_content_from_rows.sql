-- A Build that placed or returned Games rewrote its Audit's Entries to where the files went, but kept the content
-- SHA-256 recorded when the Audit finished (#385): the next Audit that found the same then made its Library Folder
-- Stale. Each finished Audit's is forgotten, and the store records it again from its rows when it opens.
UPDATE audits SET content_sha256 = NULL WHERE status = 'finished' AND pruned_at IS NULL;
