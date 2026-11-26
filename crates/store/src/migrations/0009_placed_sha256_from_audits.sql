-- A Placed file a Build moved by a rename was recorded with no SHA-256 (#373, ADR 0031), so a check that found its
-- change time moved, as any chmod or chgrp moves it, could never prove it unchanged. Each such file gets the SHA-256
-- its Audit verified, where the store can show that Audit verified it as the ROM placed: the payload read recorded
-- where it is placed, of the same file by its device, inode, size and modification time, and of all of it, a loose
-- file read with no Header left out; every checksum the Build that placed it recorded for its one ROM agreeing with
-- that read's, a SHA-1 or SHA-256 among them; and the Source Set's latest Audit Matched on Payload Evidence for that
-- ROM. A ZIP's read is of its members and says nothing of the archive's own SHA-256; it, and any other without one,
-- is checked against the ROMs it was placed as until an Audit or a check records one.
UPDATE placed_files
SET sha256 = (
    SELECT json_extract(r.members, '$[0][1].sha256')
    FROM payload_reads r
    JOIN built_files b ON b.folder_id = placed_files.folder_id AND b.target = placed_files.path
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
      AND b.loose = 1
      AND json_array_length(b.roms) = 1
      AND json_extract(b.roms, '$[0].size') = placed_files.size
      -- A SHA-1 or SHA-256 says what its bytes are, and every checksum the ROM lists agrees with the read.
      AND coalesce(json_extract(b.roms, '$[0].sha256'), json_extract(b.roms, '$[0].sha1')) IS NOT NULL
      AND coalesce(lower(json_extract(b.roms, '$[0].sha256')) = lower(json_extract(r.members, '$[0][1].sha256')), 1)
      AND coalesce(lower(json_extract(b.roms, '$[0].sha1')) = lower(json_extract(r.members, '$[0][1].sha1')), 1)
      AND coalesce(lower(json_extract(b.roms, '$[0].md5')) = lower(json_extract(r.members, '$[0][1].md5')), 1)
      AND coalesce(lower(json_extract(b.roms, '$[0].crc')) = lower(json_extract(r.members, '$[0][1].crc')), 1)
      AND EXISTS (
          SELECT 1 FROM audit_entries e
          WHERE e.audit_id = (
                  SELECT a.id FROM audits a
                  WHERE a.source_set_id = placed_files.source_set_id AND a.status = 'finished'
                  ORDER BY a.finish_seq DESC LIMIT 1
              )
            AND e.path = r.path
            AND e.member IS NULL
            AND e.finding = 'matched'
            AND e.evidence_level = 'payload'
            AND EXISTS (
                SELECT 1 FROM json_each(e.roms) m
                WHERE json_extract(m.value, '$.game') = placed_files.game
                  AND json_extract(m.value, '$.name') = json_extract(b.roms, '$[0].name')
            )
      )
)
WHERE sha256 IS NULL AND chd_roms IS NULL;
