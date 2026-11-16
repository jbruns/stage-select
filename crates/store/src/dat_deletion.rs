//! Explicit DAT Revision deletion (ADR 0003, amendment of 2026-11-15).
//!
//! The Operator may delete one DAT Revision that nothing depends on, even the newest, last or a restored one. What
//! depends on it is found by [`blockers`]; deletion never purges, prunes or unregisters anything to become eligible.
//!
//! Every reference to a DAT Revision, or to its DAT, in the schema, and whether it blocks deletion or is history:
//!
//! | Reference                                   | Blocks while                                                   | Otherwise |
//! | ------------------------------------------- | -------------------------------------------------------------- | --------- |
//! | `source_sets.dat_id`                        | registered (every revision of the DAT)                          | history   |
//! | `additions_sets.dat_id`                     | registered (every revision of the DAT)                          | history   |
//! | `audits.dat_revision_id`                    | not pruned (finished or running)                                | history   |
//! | `selections.dat_revision_id`                | not pruned                                                      | history   |
//! | `plans` (via `selection_id`, `previous_selection_id`, `audit_id`) | not built, at its registered Library Folder's generation | history |
//! | `repair_plans` (via `audit_id`)             | not finished                                                    | history   |
//! | `builds.dat_revision_id`                    | running, not cleaned up, or a registered folder's current Build | history   |
//! | `built_files.dat_revision_id`               | in a folder that isn't an Unregistered Library Folder           | history   |
//! | `kept_files.dat_revision_id`                | keeping, kept, deleting, quarantined or restoring, likewise     | history   |
//! | `jobs.dat_revision_id`                      | queued or running                                               | history   |
//! | `ingest_reaudits.sha256`                    | not fulfilled                                                   | history   |
//! | `followed_revisions.dat_revision_id`        | never                                                           | history   |
//! | `fetches.dat_revision_id`                   | never                                                           | history   |
//!
//! A deleted revision that history still names keeps its row, hidden by `deleted_at` and holding no blob, so that the
//! history keeps its identity and every foreign key still holds; one that nothing names is deleted outright, and so
//! is its DAT's row once nothing names that. A hidden DAT Revision is in no catalogue listing, never the newest of its
//! DAT, and never chosen for new work. Importing its bytes again brings it back as a fresh import.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::blobs::remove_blob;
use super::{Error, Result, Store, now};
use title_select_api::{DeletedDatRevision, DeletionBlocker, DeletionBlockerKind};

/// What still depends on the DAT Revision `id` of the DAT `dat_id`, whose SHA-256 is `sha256`, by kind, in
/// [`DeletionBlockerKind`] order; empty when it may be deleted.
pub(crate) fn blockers(
    c: &Connection,
    id: i64,
    dat_id: i64,
    sha256: &str,
) -> Result<Vec<DeletionBlocker>> {
    use DeletionBlockerKind as K;
    // Each query yields one row per name (a null name counting without naming) with how many it stands for.
    let queries: [(K, &str); 10] = [
        (
            K::SourceSet,
            "SELECT name, 1 FROM source_sets WHERE dat_id = ?2 AND unregistered_at IS NULL ORDER BY name",
        ),
        (
            K::AdditionsSet,
            "SELECT root || '/' || path, 1 FROM additions_sets
             WHERE dat_id = ?2 AND unregistered_at IS NULL ORDER BY root, path",
        ),
        (
            K::Audit,
            "SELECT s.name, count(*) FROM audits a JOIN source_sets s ON s.id = a.source_set_id
             WHERE a.dat_revision_id = ?1 AND a.pruned_at IS NULL GROUP BY s.name ORDER BY s.name",
        ),
        (
            K::Selection,
            "SELECT s.name, count(*) FROM selections sel JOIN source_sets s ON s.id = sel.source_set_id
             WHERE sel.dat_revision_id = ?1 AND sel.pruned_at IS NULL GROUP BY s.name ORDER BY s.name",
        ),
        (
            K::ChangePlan,
            "SELECT name, count(*) FROM (
                 SELECT s.name AS name FROM repair_plans p
                 JOIN audits a ON a.id = p.audit_id JOIN source_sets s ON s.id = p.source_set_id
                 WHERE a.dat_revision_id = ?1 AND p.finished_at IS NULL
                 UNION ALL
                 SELECT f.root || '/' || f.path FROM plans p JOIN library_folders f ON f.id = p.folder_id
                 WHERE f.unregistered_at IS NULL AND p.generation = f.generation
                   AND NOT EXISTS (SELECT 1 FROM builds b WHERE b.plan_id = p.id)
                   AND (EXISTS (SELECT 1 FROM selections sel
                                WHERE sel.id IN (p.selection_id, p.previous_selection_id)
                                  AND sel.dat_revision_id = ?1)
                        OR EXISTS (SELECT 1 FROM audits a WHERE a.id = p.audit_id AND a.dat_revision_id = ?1)))
             GROUP BY name ORDER BY name",
        ),
        (
            K::Build,
            "SELECT f.root || '/' || f.path, count(*) FROM builds b LEFT JOIN library_folders f ON f.id = b.folder_id
             WHERE b.dat_revision_id = ?1
               AND (b.state = 'running' OR b.cleaned_at IS NULL
                    OR (f.current_build_id = b.id AND f.unregistered_at IS NULL))
             GROUP BY f.id ORDER BY f.root, f.path",
        ),
        (
            K::BuiltFile,
            "SELECT f.root || '/' || f.path, count(*) FROM built_files b LEFT JOIN library_folders f ON f.id = b.folder_id
             WHERE b.dat_revision_id = ?1 AND f.unregistered_at IS NULL
             GROUP BY b.folder_id ORDER BY f.root, f.path",
        ),
        (
            K::KeptFile,
            "SELECT f.root || '/' || f.path, count(*) FROM kept_files k LEFT JOIN library_folders f ON f.id = k.folder_id
             WHERE k.dat_revision_id = ?1 AND f.unregistered_at IS NULL
               AND k.state IN ('keeping', 'kept', 'deleting', 'quarantined', 'restoring')
             GROUP BY k.folder_id ORDER BY f.root, f.path",
        ),
        (
            K::Job,
            "SELECT NULL, count(*) FROM jobs WHERE dat_revision_id = ?1 AND state IN ('queued', 'running')
             HAVING count(*) > 0",
        ),
        (
            K::IngestReaudit,
            "SELECT s.name, count(*) FROM ingest_reaudits o JOIN source_sets s ON s.id = o.source_set_id
             WHERE o.sha256 = ?3 AND o.fulfilled_at IS NULL GROUP BY s.name ORDER BY s.name",
        ),
    ];
    let mut found = Vec::new();
    for (kind, sql) in queries {
        let mut stmt = c.prepare_cached(sql)?;
        // Each query binds the leading parameters up to the last it names.
        let all = params![id, dat_id, sha256];
        let bound = &all[..stmt.parameter_count()];
        let rows = stmt
            .query_map(bound, |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let count: i64 = rows.iter().map(|(_, n)| n).sum();
        if count > 0 {
            found.push(DeletionBlocker {
                kind,
                count: count as u64,
                names: rows.into_iter().filter_map(|(name, _)| name).collect(),
            });
        }
    }
    Ok(found)
}

/// The refusal to delete `revision` (named for you) while `blockers` depend on it.
fn in_use(revision: &str, blockers: Vec<DeletionBlocker>) -> Error {
    let why: Vec<String> = blockers.iter().map(DeletionBlocker::describe).collect();
    Error::RevisionInUse {
        message: format!(
            "you can't delete DAT revision {revision} yet: {}. Nothing was changed",
            why.join("; ")
        ),
        blockers,
    }
}

/// Runs `sql` with `params` under a savepoint, keeping its effect only if no foreign key still names the row it
/// deletes; whether it did.
fn delete_unless_named(c: &Connection, sql: &str, id: i64) -> Result<bool> {
    c.execute_batch("SAVEPOINT unnamed_delete")?;
    match c.execute(sql, [id]) {
        Ok(_) => {
            c.execute_batch("RELEASE unnamed_delete")?;
            Ok(true)
        }
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY =>
        {
            c.execute_batch("ROLLBACK TO unnamed_delete; RELEASE unnamed_delete")?;
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

impl Store {
    /// Deletes the catalogued DAT Revision `sha256` if nothing depends on it ([`blockers`], checked again in the
    /// writer transaction that deletes it), then its blob if present, as [`Store::retain_blobs`] drops one: the row
    /// first, so a crash leaves only garbage. Refused with [`Error::RevisionInUse`], changing nothing, while anything
    /// does. Touches no file but its blob.
    pub fn delete_dat_revision(&self, sha256: &str) -> Result<DeletedDatRevision> {
        self.with_writer(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (id, dat_id, blob_present, dat, version): (i64, i64, bool, String, String) = tx
                .query_row(
                    "SELECT r.id, r.dat_id, r.blob_dropped_at IS NULL, d.provider || '/' || d.name, r.version
                     FROM dat_revisions r JOIN dats d ON d.id = r.dat_id
                     WHERE r.sha256 = ?1 AND r.deleted_at IS NULL",
                    [sha256],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?
                .ok_or_else(|| Error::NotFound(format!("no DAT revision {sha256}")))?;
            let found = blockers(&tx, id, dat_id, sha256)?;
            if !found.is_empty() {
                let named = if version.is_empty() {
                    dat
                } else {
                    format!("{dat} {version}")
                };
                return Err(in_use(&named, found));
            }
            let at = now();
            tx.execute(
                "UPDATE dat_revisions
                 SET deleted_at = ?2, blob_dropped_at = coalesce(blob_dropped_at, ?2), restored_at = NULL,
                     header_order = 0
                 WHERE id = ?1",
                params![id, at],
            )?;
            delete_unless_named(&tx, "DELETE FROM dat_revisions WHERE id = ?1", id)?;
            crate::dats::reorder_revisions(&tx, dat_id)?;
            let remaining: i64 = tx.query_row(
                "SELECT count(*) FROM dat_revisions WHERE dat_id = ?1 AND deleted_at IS NULL",
                [dat_id],
                |r| r.get(0),
            )?;
            let dat_removed = remaining == 0;
            if dat_removed {
                let hidden: i64 = tx.query_row(
                    "SELECT count(*) FROM dat_revisions WHERE dat_id = ?1",
                    [dat_id],
                    |r| r.get(0),
                )?;
                if hidden == 0 {
                    delete_unless_named(&tx, "DELETE FROM dats WHERE id = ?1", dat_id)?;
                }
            }
            tx.commit()?;
            // The row no longer holds the blob, and the writer is still held, so no import can restore it in between.
            if blob_present {
                remove_blob(&self.blob_path(sha256))?;
            }
            Ok(DeletedDatRevision {
                sha256: sha256.to_owned(),
                dat,
                blob_deleted: blob_present,
                dat_removed,
            })
        })
    }
}
