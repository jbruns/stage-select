//! Deleting a Library Folder: one plan that removes every built file, as removing them all does, and then deletes
//! what it kept aside and the folder's other kept files, removes the folder itself when nothing Stage Select didn't
//! write is left in it, and unregisters it last. A Game placed in the folder under Single Copy is returned to its
//! Source Set's folder by the plan's Build, never deleted. The deletion is recorded in the transaction that approves
//! the plan, carried out once its Build succeeds, and finished when the service next starts if it stopped first.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use rustix::fs::FileType;
use stage_select_api::build::{
    FolderDeletion, FolderDeletionOutcome, FolderLeftover, KeptFile, MARKER, Plan, PlanBuild,
    PlanContent, PlanKeptDeletion, RETENTION, RemovalKind, STATE,
};
use stage_select_api::lifecycle::RegistrationKind;

use crate::folder_dir::FolderDir;
use crate::folders::{folder_by_id, open_folder, store_id};
use crate::kept::generation_dir;
use crate::lifecycle::{holds_of, remove_empty_dirs, remove_own_marker, unregister_in};
use crate::{Error, Fault, Result, Store, now};

/// Why an entry outside the folder's own directory is left.
const NOT_WRITTEN: &str = "Stage Select didn't write it";

/// Records, in the transaction that approves plan `plan` by `actor`, that it deletes Library Folder `folder`.
pub(crate) fn record(tx: &Transaction, folder: i64, plan: i64, actor: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO folder_deletions (plan_id, folder_id, actor) VALUES (?1, ?2, ?3)",
        params![plan, folder, actor],
    )?;
    Ok(())
}

/// Why a change to Library Folder `folder` waits: an approved deletion of it isn't finished.
pub(crate) fn unfinished(c: &Connection, folder: i64) -> Result<Option<String>> {
    let plan: Option<i64> = c
        .query_row(
            "SELECT plan_id FROM folder_deletions WHERE folder_id = ?1 AND finished_at IS NULL",
            [folder],
            |r| r.get(0),
        )
        .optional()?;
    Ok(plan.map(|plan| {
        format!(
            "plan {plan} deletes this play library folder and isn't finished; the service finishes it when it next \
             starts"
        )
    }))
}

/// What deleting the folder did, from the plan query's columns from `at`: none unless the plan deletes it.
pub(crate) fn outcome_from_row(
    r: &Row,
    at: usize,
) -> rusqlite::Result<Option<FolderDeletionOutcome>> {
    if r.get::<_, Option<i64>>(at)?.is_none() {
        return Ok(None);
    }
    let left: Option<String> = r.get(at + 3)?;
    Ok(Some(FolderDeletionOutcome {
        finished_at: r.get(at + 1)?,
        removed: r.get(at + 2)?,
        left: left
            .and_then(|l| serde_json::from_str(&l).ok())
            .unwrap_or_default(),
        detail: r.get(at + 4)?,
    }))
}

/// The path of kept file `kept` in its folder.
fn kept_path(kept: &KeptFile) -> String {
    format!("{}/{}", generation_dir(kept.generation), kept.kept_name)
}

/// What a deletion expects to find in the folder, and deletes, and what it knows of what it leaves.
#[derive(Default)]
struct Expected {
    /// Files it deletes, by path in the folder.
    files: BTreeSet<String>,
    /// Kept directories, which Stage Select never deletes: each is left whole.
    kept_dirs: BTreeSet<String>,
    /// Why each built file it leaves is left, by path.
    reasons: BTreeMap<String, String>,
}

impl Expected {
    /// What plan `content`, which deletes the folder, finds there before its Build runs: the built files it keeps
    /// aside, the placed files it returns, and the folder's kept files `kept`.
    fn before(content: &PlanContent, kept: &[KeptFile]) -> Expected {
        let mut expected = Expected::after(content, kept);
        expected.files.extend(
            content
                .removals
                .iter()
                .filter(|r| r.keep.is_some())
                .map(|r| r.target.clone()),
        );
        expected.files.extend(
            content
                .returns
                .iter()
                .flat_map(|r| &r.placed)
                .filter(|p| !p.keeps)
                .map(|p| p.path.clone()),
        );
        expected
            .files
            .extend(kept.iter().filter(|k| !k.directory).map(kept_path));
        expected
    }

    /// What is known of the folder once plan `content`'s Build ran and the folder's kept files were deleted: what of
    /// it is left, and why.
    fn after(content: &PlanContent, kept: &[KeptFile]) -> Expected {
        Expected {
            files: BTreeSet::new(),
            kept_dirs: kept.iter().filter(|k| k.directory).map(kept_path).collect(),
            reasons: content
                .removals
                .iter()
                .filter(|r| r.kind == RemovalKind::Leave)
                .map(|r| {
                    (
                        r.target.clone(),
                        "it changed since Stage Select built it, so it isn't the file it wrote"
                            .to_owned(),
                    )
                })
                .collect(),
        }
    }

    fn why(&self, path: &str) -> String {
        if let Some(why) = self.reasons.get(path) {
            why.clone()
        } else if path.starts_with(&format!("{RETENTION}/")) {
            "it is in the folder's kept files area, but isn't a kept file Stage Select can delete"
                .into()
        } else if path.starts_with(&format!("{STATE}/")) {
            "it is unfinished work of Stage Select's; plan again once the service has settled the folder".into()
        } else {
            NOT_WRITTEN.into()
        }
    }
}

/// Each entry under `dir`, at `prefix` in the folder, that deleting the folder leaves: every file other than the
/// marker that isn't `expected`, and each kept directory, whole. A directory that would be left empty is removed, so
/// it isn't listed.
fn leftovers(
    dir: &FolderDir,
    prefix: &str,
    expected: &Expected,
    left: &mut Vec<FolderLeftover>,
) -> std::io::Result<()> {
    let mut entries = dir.entries()?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, kind) in entries {
        let path = match prefix {
            "" => name.clone(),
            prefix => format!("{prefix}/{name}"),
        };
        if kind == FileType::Directory {
            if expected.kept_dirs.contains(&path) {
                left.push(FolderLeftover {
                    name: path,
                    why: "it is a kept folder, which Stage Select never deletes".into(),
                });
            } else {
                leftovers(&dir.sub(&name)?, &path, expected, left)?;
            }
        } else if path != MARKER && !expected.files.contains(&path) {
            left.push(FolderLeftover {
                why: expected.why(&path),
                name: path,
            });
        }
    }
    Ok(())
}

impl Store {
    /// Plans deleting Library Folder `folder`, from the Selection of its current Build, and stores the plan for
    /// review: every built file kept aside, each Placed Game returned to its Source Set's folder, and then the files
    /// kept aside and the folder's other kept files deleted, the folder removed unless something Stage Select didn't
    /// write is left in it, and the folder unregistered.
    pub fn plan_folder_deletion(&self, folder: i64) -> Result<Plan> {
        let selection: Option<i64> = self.read(|c| {
            folder_by_id(c, folder)?.check_registered()?;
            Ok(c.query_row(
                "SELECT b.selection_id FROM library_folders f JOIN builds b ON b.id = f.current_build_id
                 WHERE f.id = ?1",
                [folder],
                |r| r.get(0),
            )
            .optional()?)
        })?;
        let selection = selection.ok_or_else(|| {
            Error::Refused(
                "nothing was built in this play library folder, so there is nothing to delete; unregister it instead"
                    .into(),
            )
        })?;
        let mut request = PlanBuild::new(selection);
        request.choices.remove_all = true;
        request.choices.delete_folder = true;
        self.plan_build_with(folder, &request)
    }

    /// What plan `content`, which deletes Library Folder `id` at `path` in Root `root`, deletes besides the files it
    /// keeps aside, and what it would leave in the folder as it is now.
    pub(crate) fn folder_deletion_of(
        &self,
        root: &str,
        path: &str,
        id: i64,
        content: &PlanContent,
    ) -> Result<FolderDeletion> {
        let kept: Vec<KeptFile> = self
            .read(|c| crate::kept::kept_where(c, "k.folder_id = ?1 AND k.state = 'kept'", [id]))?;
        let mut left = Vec::new();
        // A folder that can't be opened blocks the plan already.
        if let Ok(dir) = open_folder(&self.roots(), root, path) {
            leftovers(&dir, "", &Expected::before(content, &kept), &mut left)
                .map_err(|e| Error::Io(format!("{root}/{path}: {e}")))?;
        }
        Ok(FolderDeletion {
            kept: kept.into_iter().filter(|k| !k.directory).collect(),
            left,
        })
    }

    /// Carries out the deletion of its folder that plan `plan`'s approval recorded, unless it is finished: once its
    /// Build `succeeded`, deletes the folder's kept files as an approved deletion of them by whoever approved the
    /// plan, removes the folder when nothing else is left in it, and unregisters it. A Build that didn't succeed
    /// finishes it with nothing of the folder deleted. Left unfinished, to finish when the service next starts, while
    /// another change to the folder's kept files is under way or the folder can't be opened.
    pub(crate) fn delete_folder(&self, plan: i64, succeeded: bool) -> Result<()> {
        let pending: Option<(i64, String, Option<String>)> = self.read(|c| {
            Ok(c.query_row(
                "SELECT folder_id, actor, removing_at FROM folder_deletions
                 WHERE plan_id = ?1 AND finished_at IS NULL",
                [plan],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
        })?;
        let Some((folder, actor, removing)) = pending else {
            return Ok(());
        };
        if !succeeded {
            return self.finish_folder_deletion(
                plan,
                folder,
                false,
                &[],
                Some("its build didn't finish, so nothing of the folder was deleted and it stays registered".into()),
                false,
            );
        }
        if removing.is_none() {
            self.fault_point(Fault::FolderDeleting);
            self.delete_all_kept(folder, &actor)?;
        }
        let _lifecycle = self.lifecycle_lock();
        self.remove_folder(&self.plan(plan)?, folder, removing.is_some())
    }

    /// Deletes every kept file of Library Folder `folder`, by `actor`. One that is no longer the file kept stays, and
    /// is left in the folder.
    fn delete_all_kept(&self, folder: i64, actor: &str) -> Result<()> {
        if self.kept_files(folder)?.iter().all(|k| k.directory) {
            return Ok(());
        }
        let all = PlanKeptDeletion {
            ids: Vec::new(),
            all: true,
        };
        let deletion = match self.plan_kept_deletion(folder, &all) {
            Ok(deletion) => deletion,
            Err(Error::Refused(_)) => return Ok(()),
            Err(err) => return Err(err),
        };
        match self.approve_kept_deletion(deletion.id, folder, &deletion.digest, actor) {
            Ok(_) | Err(Error::Refused(_)) => Ok(()),
            Err(err) => Err(err),
        }
    }

    /// With the lifecycle lock held: removes Library Folder `folder` once nothing but its marker and empty
    /// directories is left in it, and unregisters it. `removing` is whether removing it had begun before a stop.
    fn remove_folder(&self, plan: &Plan, folder: i64, removing: bool) -> Result<()> {
        let roots = self.roots();
        let (row, store, holds, kept) = self.read(|c| {
            Ok((
                folder_by_id(c, folder)?,
                store_id(c)?,
                holds_of(c, RegistrationKind::LibraryFolder, folder)?,
                crate::kept::kept_where(c, "k.folder_id = ?1 AND k.state = 'kept'", [folder])?,
            ))
        })?;
        if row.unregistered_at.is_none() && !holds.is_empty() {
            return self.finish_folder_deletion(
                plan.id,
                folder,
                false,
                &[],
                Some(format!(
                    "the folder can't be unregistered yet, so it was left: {}. Unregister it once nothing holds it",
                    holds.join("; ")
                )),
                false,
            );
        }
        let (parent, base) = match row.path.rsplit_once('/') {
            Some((parent, base)) => (parent, base),
            None => ("", row.path.as_str()),
        };
        let gone = || {
            let root = roots.root(&row.root)?;
            let parent = FolderDir::open(&root.path, parent).ok()?;
            matches!(parent.stat(base), Ok(None)).then_some(())
        };
        let dir = match open_folder(&roots, &row.root, &row.path) {
            Ok(dir) => dir,
            // Found gone once removing it had begun: this deletion removed it before a stop.
            Err(_) if removing && gone().is_some() => {
                return self.finish_folder_deletion(plan.id, folder, true, &[], None, true);
            }
            Err(why) => {
                return Err(Error::Conflict(format!(
                    "plan {}'s deletion of the folder is waiting: the folder can't be opened: {why}",
                    plan.id
                )));
            }
        };
        let mut left = Vec::new();
        leftovers(&dir, "", &Expected::after(&plan.content, &kept), &mut left)
            .map_err(|e| Error::Io(format!("{}/{}: {e}", row.root, row.path)))?;
        if !left.is_empty() {
            self.finish_folder_deletion(plan.id, folder, false, &left, None, true)?;
            if let Some(note) = self.settle_marker_in(RegistrationKind::LibraryFolder, folder)? {
                self.write(|tx| {
                    tx.execute(
                        "UPDATE folder_deletions SET detail = ?2 WHERE plan_id = ?1",
                        params![plan.id, note],
                    )?;
                    Ok(())
                })?;
            }
            return Ok(());
        }
        if !removing {
            self.write(|tx| {
                tx.execute(
                    "UPDATE folder_deletions SET removing_at = ?2 WHERE plan_id = ?1",
                    params![plan.id, now()],
                )?;
                Ok(())
            })?;
        }
        self.fault_point(Fault::FolderRemoving);
        remove_empty_dirs(&dir);
        if let Err(why) = remove_own_marker(&dir, &store, RegistrationKind::LibraryFolder, folder) {
            return self.finish_folder_deletion(plan.id, folder, false, &[], Some(why), true);
        }
        drop(dir);
        let removed = match roots.root(&row.root) {
            None => Err(format!("there is no root {} now", row.root)),
            Some(_) if row.path.is_empty() => {
                Err("it is its root's own folder, which Stage Select doesn't remove".into())
            }
            Some(root) => FolderDir::open(&root.path, parent)
                .and_then(|parent| {
                    parent.rmdir(base)?;
                    parent.sync()
                })
                .map_err(|e| e.to_string()),
        };
        match removed {
            Ok(()) => self.finish_folder_deletion(plan.id, folder, true, &[], None, true),
            Err(why) => self.finish_folder_deletion(
                plan.id,
                folder,
                false,
                &[],
                Some(format!(
                    "the folder is empty, but it couldn't be removed: {why}"
                )),
                true,
            ),
        }
    }

    /// Finishes plan `plan`'s deletion of Library Folder `folder` with what it did, unregistering the folder in the
    /// same write when `unregister` is set and it isn't already.
    fn finish_folder_deletion(
        &self,
        plan: i64,
        folder: i64,
        removed: bool,
        left: &[FolderLeftover],
        detail: Option<String>,
        unregister: bool,
    ) -> Result<()> {
        self.write(|tx| {
            let mut detail = detail.clone();
            if unregister {
                match unregister_in(tx, RegistrationKind::LibraryFolder, folder) {
                    Ok(_) | Err(Error::Refused(_)) => {}
                    Err(Error::Conflict(why)) => detail = Some(why),
                    Err(err) => return Err(err),
                }
            }
            let left = (!left.is_empty()).then(|| crate::plans::to_json(&left));
            tx.execute(
                "UPDATE folder_deletions SET finished_at = ?2, removed = ?3, left = ?4, detail = ?5
                 WHERE plan_id = ?1",
                params![plan, now(), removed, left, detail],
            )?;
            Ok(())
        })
    }

    /// Run when the service starts, after [`Store::recover_removal_deletions`]: finishes each deletion of a Library
    /// Folder the service stopped before finishing, once its plan's Build succeeded; one whose Build didn't is
    /// finished with nothing of the folder deleted, and one whose Build is still to run is left to it. Returns, for
    /// each left unfinished, its plan and why.
    pub fn recover_folder_deletions(&self) -> Result<Vec<(i64, String)>> {
        let pending: Vec<(i64, bool, Option<String>)> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT d.plan_id, b.id IS NOT NULL, j.state FROM folder_deletions d
                 LEFT JOIN jobs j ON j.plan_id = d.plan_id AND j.kind = 'build'
                 LEFT JOIN builds b ON b.job_id = j.id AND b.state = 'succeeded'
                 WHERE d.finished_at IS NULL ORDER BY d.plan_id",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        let mut unfinished = Vec::new();
        for (plan, succeeded, job) in pending {
            if !succeeded && matches!(job.as_deref(), Some("queued" | "running")) {
                continue;
            }
            match self.delete_folder(plan, succeeded) {
                Ok(()) => {}
                Err(Error::Conflict(why)) => unfinished.push((plan, why)),
                Err(err) => return Err(err),
            }
        }
        Ok(unfinished)
    }
}
