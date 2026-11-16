-- Explicit DAT Revision deletion (#168, ADR 0003 amendment of 2026-11-15). A deleted revision that history still
-- names (a pruned Audit or Selection, a finished Job or Build, a fetch, a followed revision) keeps its row, hidden from
-- the catalogue, so that history keeps its identity and every foreign key still holds. One nothing names is deleted
-- outright. Importing the same bytes again brings a hidden row back as a fresh import.
ALTER TABLE dat_revisions ADD COLUMN deleted_at TEXT;
