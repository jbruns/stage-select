-- DAT metadata (#165): what a DAT Revision's DAT says about itself, read at import. A DAT is keyed by the DAT
-- Provider recognised from its format and header, or one fetched from, else by a provider part reserved for its DAT
-- Format (`unknown-logiqx`, ...). Earlier revisions are not backfilled (#164): they read as Logiqx with no claims.

-- The DAT Format: `logiqx`, `listxml` or `software_list`.
ALTER TABLE dat_revisions ADD COLUMN format TEXT NOT NULL DEFAULT 'logiqx';
-- What the header claims, as written; null when it doesn't say. header_url is the header's own `url`, apart from the
-- source_url it was downloaded from.
ALTER TABLE dat_revisions ADD COLUMN description TEXT;
ALTER TABLE dat_revisions ADD COLUMN author TEXT;
ALTER TABLE dat_revisions ADD COLUMN homepage TEXT;
ALTER TABLE dat_revisions ADD COLUMN header_url TEXT;
