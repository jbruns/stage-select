-- An Audit's content SHA-256 no longer counts where it found each file, nor where else a missing ROM is (#385): a
-- Build that placed or returned Games rewrote its Audit's Entries to where the files went, and the next Audit that
-- found the same made a Library Folder Stale. Each finished Audit's is forgotten, and the store records it again from
-- its rows when it opens.
UPDATE audits SET content_sha256 = NULL WHERE status = 'finished' AND pruned_at IS NULL;
