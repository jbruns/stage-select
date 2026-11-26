-- A Placed file a Build moved by a rename was recorded with no SHA-256 (#373, ADR 0031), so a check that found its
-- change time moved, as any chmod or chgrp moves it, could never prove it unchanged. Each such file gets the SHA-256
-- its Audit verified, where the store holds one: the payload read recorded where it is placed, of the same file by
-- its device, inode, size and modification time, and of all of it, a loose file read with no Header left out. A
-- ZIP's read is of its members and says nothing of the archive's own SHA-256; it, and any other without one, is
-- checked against its DAT's ROMs until an Audit or a check records one.
UPDATE placed_files
SET sha256 = (
    SELECT json_extract(r.members, '$[0][1].sha256')
    FROM payload_reads r
    WHERE r.source_set_id = placed_files.source_set_id
      AND r.path = '.stage-select/placed/' || placed_files.folder_id || '/' || placed_files.path
      AND r.dev = placed_files.dev
      AND r.ino = placed_files.ino
      AND r.size = placed_files.size
      AND r.mtime_ns = placed_files.mtime_ns
      AND r.disk_sha1 IS NULL
      AND json_array_length(r.members) = 1
      AND json_type(r.members, '$[0][0]') = 'null'
      AND json_type(r.members, '$[0][1].header') IS NULL
      AND json_extract(r.members, '$[0][1].size') = placed_files.size
)
WHERE sha256 IS NULL AND chd_roms IS NULL;
