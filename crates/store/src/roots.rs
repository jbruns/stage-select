//! Roots: the folders Title Select may read, registered, edited and unregistered by the Operator in the store
//! (ADR 0029), and held in memory as a snapshot that each change replaces, so that every reader sees a change at once
//! without a restart and none waits on the database to find a Root.
//!
//! Every rule a Root keeps is checked here, for the API and the CLI alike. Its name is fixed once registered, as every
//! record of a folder in it refers to it by name. Its path or writability doesn't change while a Job runs on it; its
//! path doesn't change while approved work still writes to it; and making it less writable voids each approved plan,
//! not yet started, that writes where it may no longer write. Every change is recorded with who made it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use title_select_api::paths::relative_path;
use title_select_api::roots::{RootChange, RootEdited, RootSpec, VoidedPlan, VoidedPlanKind};
use title_select_api::settings::{MAX_MIB_PER_S, MAX_WORKERS, RootOverrides};

pub use title_select_api::roots::RootKind;

use crate::{Error, Result, Store, now};

/// A registered Root, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub name: String,
    pub path: PathBuf,
    pub kind: RootKind,
    /// Whether Title Select may write to this Root: Builds to its Library Folders, and repairs to any Source Set in it.
    /// A Source Set is registered only in a read-only Root.
    pub writable: bool,
    /// For a read-only Root: the paths, relative to it, of the Source Sets in it that a repair may write to.
    pub writable_paths: Vec<String>,
    /// The Operator's Ceilings for it (ADR 0022).
    pub readers: Option<u64>,
    pub read_mib_per_s: Option<u64>,
    pub write_mib_per_s: Option<u64>,
    pub updated_by: String,
    pub updated_at: String,
}

impl Root {
    /// A read-only general Root `name` at `path`, with no Ceilings of its own, never stored.
    #[cfg(test)]
    pub(crate) fn test(name: &str, path: impl Into<PathBuf>) -> Root {
        Root {
            name: name.into(),
            path: path.into(),
            kind: RootKind::General,
            writable: false,
            writable_paths: Vec::new(),
            readers: None,
            read_mib_per_s: None,
            write_mib_per_s: None,
            updated_by: "test".into(),
            updated_at: "then".into(),
        }
    }

    pub fn is_ingest(&self) -> bool {
        self.kind == RootKind::Ingest
    }

    /// Whether Title Select may write to the folder `rel` of this Root: the Root is writable, or lists `rel` among
    /// its `writable_paths`.
    pub fn may_write(&self, rel: &str) -> bool {
        self.writable
            || relative_path(rel).is_ok_and(|rel| self.writable_paths.contains(&rel))
    }

    /// The Operator's Ceilings for it.
    pub fn overrides(&self) -> RootOverrides {
        RootOverrides {
            readers: self.readers,
            read_mib_per_s: self.read_mib_per_s,
            write_mib_per_s: self.write_mib_per_s,
        }
    }

    /// As the API returns it.
    pub fn view(&self) -> title_select_api::Root {
        title_select_api::Root {
            name: self.name.clone(),
            path: self.path.display().to_string(),
            kind: self.kind,
            writable: self.writable,
            writable_paths: self.writable_paths.clone(),
            readers: self.readers,
            read_mib_per_s: self.read_mib_per_s,
            write_mib_per_s: self.write_mib_per_s,
            updated_by: self.updated_by.clone(),
            updated_at: self.updated_at.clone(),
        }
    }

    /// What it would let Title Select write to, as `target` needs.
    fn allows(&self, target: &Target) -> bool {
        match target {
            Target::Whole => self.writable,
            Target::Set(rel) => self.may_write(rel),
        }
    }
}

/// Every registered Root, by name, as it was when last changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    by_name: BTreeMap<String, Root>,
}

impl Roots {
    pub fn root(&self, name: &str) -> Option<&Root> {
        self.by_name.get(name)
    }

    /// Every Root, by name.
    pub fn iter(&self) -> impl Iterator<Item = &Root> {
        self.by_name.values()
    }

    /// The Root of kind `additions`, which holds the Additions Sets, if one is registered.
    pub fn additions(&self) -> Option<&Root> {
        self.iter().find(|r| r.kind == RootKind::Additions)
    }

    /// Every Root that is an Ingest Folder.
    pub fn ingest(&self) -> impl Iterator<Item = &Root> {
        self.iter().filter(|r| r.is_ingest())
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// A snapshot of `roots`, for tests and tools that work without a store.
    pub fn of(roots: impl IntoIterator<Item = Root>) -> Roots {
        Roots {
            by_name: roots.into_iter().map(|r| (r.name.clone(), r)).collect(),
        }
    }
}

const COLUMNS: &str = "name, path, kind, writable, writable_paths, readers, read_mib_per_s, write_mib_per_s, \
                       updated_by, updated_at";

fn row(r: &rusqlite::Row) -> rusqlite::Result<(Root, String)> {
    let kind: String = r.get(2)?;
    Ok((
        Root {
            name: r.get(0)?,
            path: PathBuf::from(r.get::<_, String>(1)?),
            kind: kind.parse().unwrap_or_default(),
            writable: r.get(3)?,
            writable_paths: Vec::new(),
            readers: r.get::<_, Option<i64>>(5)?.map(|v| v as u64),
            read_mib_per_s: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
            write_mib_per_s: r.get::<_, Option<i64>>(7)?.map(|v| v as u64),
            updated_by: r.get(8)?,
            updated_at: r.get(9)?,
        },
        r.get(4)?,
    ))
}

fn with_paths((mut root, paths): (Root, String)) -> Result<Root> {
    root.writable_paths = serde_json::from_str(&paths).map_err(|e| {
        Error::Io(format!(
            "the stored writable paths of root {} do not read: {e}",
            root.name
        ))
    })?;
    Ok(root)
}

/// Every Root as stored.
pub(crate) fn load(c: &Connection) -> Result<Roots> {
    let rows = c
        .prepare(&format!("SELECT {COLUMNS} FROM roots ORDER BY name"))?
        .query_map([], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Roots::of(
        rows.into_iter().map(with_paths).collect::<Result<Vec<_>>>()?,
    ))
}

fn stored(c: &Connection, name: &str) -> Result<Option<Root>> {
    c.query_row(
        &format!("SELECT {COLUMNS} FROM roots WHERE name = ?1"),
        [name],
        row,
    )
    .optional()?
    .map(with_paths)
    .transpose()
}

/// A Root as given, its syntax checked: what is checked against the store and the filesystem next.
struct Checked {
    name: String,
    path: PathBuf,
    kind: RootKind,
    writable: bool,
    writable_paths: Vec<String>,
    overrides: RootOverrides,
}

fn refused<T>(why: impl Into<String>) -> Result<T> {
    Err(Error::Refused(why.into()))
}

/// Checks the rules a Root keeps on its own: a name, an absolute path, a kind its writability suits, and Ceilings in
/// range.
fn checked(spec: &RootSpec) -> Result<Checked> {
    let name = spec.name.as_str();
    if name.trim().is_empty() {
        return refused("Give the root a name.");
    }
    if name.trim() != name || name.contains('/') || name.chars().any(char::is_control) {
        return refused(format!(
            "{name:?} can't be a root's name: it can't contain / or control characters, or start or end with a space."
        ));
    }
    let path = Path::new(spec.path.as_str());
    if !path.is_absolute() {
        return refused(format!(
            "Root {name}: {:?} is not an absolute path.",
            spec.path
        ));
    }
    let writable = spec.writable;
    match spec.kind {
        RootKind::Ingest if !writable => {
            return refused(format!(
                "Root {name} is an ingest folder, which must be writable."
            ));
        }
        RootKind::Additions if !writable => {
            return refused(format!(
                "Root {name} holds the additions sets, which must be writable."
            ));
        }
        _ => {}
    }
    if !spec.writable_paths.is_empty() && writable {
        return refused(format!(
            "Root {name} is writable already: writable paths are only for a read-only root."
        ));
    }
    let mut writable_paths = Vec::new();
    for rel in &spec.writable_paths {
        let rel = relative_path(rel)
            .map_err(|e| Error::Refused(format!("Root {name}: {rel:?}: a writable path {e}.")))?;
        if !writable_paths.contains(&rel) {
            writable_paths.push(rel);
        }
    }
    let in_range = |value: Option<u64>, what: &str, max: u64| match value {
        Some(v) if !(1..=max).contains(&v) => refused(format!(
            "Set {what} of root {name} to a whole number from 1 to {max}, or leave it empty to use the default."
        )),
        _ => Ok(value),
    };
    let overrides = RootOverrides {
        readers: in_range(spec.readers, "the readers", MAX_WORKERS)?,
        read_mib_per_s: in_range(spec.read_mib_per_s, "the read rate", MAX_MIB_PER_S)?,
        write_mib_per_s: in_range(spec.write_mib_per_s, "the write rate", MAX_MIB_PER_S)?,
    };
    Ok(Checked {
        name: name.to_owned(),
        path: path.to_path_buf(),
        kind: spec.kind,
        writable,
        writable_paths,
        overrides,
    })
}

/// Checks that `root`'s folder is there, and that Title Select can write to it if it is to.
fn check_folder(root: &Checked) -> Result<()> {
    let name = &root.name;
    let path = root.path.display();
    match std::fs::metadata(&root.path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return refused(format!("Root {name}: {path} is not a folder.")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return refused(format!(
                "Root {name}: {path} doesn't exist. Make the folder, or check the path."
            ));
        }
        Err(e) => return refused(format!("Root {name}: {path} can't be read: {e}.")),
    }
    if root.writable
        && let Err(e) = rustix::fs::access(&root.path, rustix::fs::Access::WRITE_OK)
    {
        return refused(format!(
            "Root {name} is to be writable, but Title Select can't write to {path}: {e}."
        ));
    }
    Ok(())
}

fn refuse_second_additions(c: &Connection, root: &Checked) -> Result<()> {
    if root.kind != RootKind::Additions {
        return Ok(());
    }
    let other: Option<String> = c
        .query_row(
            "SELECT name FROM roots WHERE kind = 'additions' AND name <> ?1",
            [&root.name],
            |r| r.get(0),
        )
        .optional()?;
    match other {
        Some(other) => refused(format!(
            "Root {other} already holds the additions sets, and there can be only one."
        )),
        None => Ok(()),
    }
}

/// Writes `root` as it now is, as `actor`, and records the change.
fn save(tx: &Transaction, root: &Checked, action: &str, actor: &str) -> Result<Root> {
    let at = now();
    let paths = serde_json::to_string(&root.writable_paths).expect("paths serialize");
    tx.execute(
        "INSERT INTO roots (name, path, kind, writable, writable_paths, readers, read_mib_per_s, write_mib_per_s,
                            updated_by, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (name) DO UPDATE SET path = ?2, kind = ?3, writable = ?4, writable_paths = ?5, readers = ?6,
             read_mib_per_s = ?7, write_mib_per_s = ?8, updated_by = ?9, updated_at = ?10",
        params![
            root.name,
            root.path.display().to_string(),
            root.kind.as_str(),
            root.writable,
            paths,
            root.overrides.readers.map(|v| v as i64),
            root.overrides.read_mib_per_s.map(|v| v as i64),
            root.overrides.write_mib_per_s.map(|v| v as i64),
            actor,
            at,
        ],
    )?;
    let saved = stored(tx, &root.name)?.expect("the root was just saved");
    record(tx, &root.name, action, Some(&saved), actor, &at)?;
    Ok(saved)
}

fn record(
    tx: &Transaction,
    name: &str,
    action: &str,
    after: Option<&Root>,
    actor: &str,
    at: &str,
) -> Result<()> {
    let after = after.map(|r| serde_json::to_string(&r.view()).expect("a root serializes"));
    tx.execute(
        "INSERT INTO root_changes (root, action, root_after, changed_by, changed_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![name, action, after, actor, at],
    )?;
    Ok(())
}

/// Each registration still registered in Root `name`, as a hold.
pub(crate) fn registrations_in(c: &Connection, name: &str) -> Result<Vec<String>> {
    let mut holds = Vec::new();
    for sql in [
        "SELECT 'source set ' || name || ' is registered in it'
         FROM source_sets WHERE root = ?1 AND unregistered_at IS NULL ORDER BY name",
        "SELECT 'play library folder ' || root || '/' || path || ' is registered in it'
         FROM library_folders WHERE root = ?1 AND unregistered_at IS NULL ORDER BY path",
        "SELECT 'the additions set of ' || d.provider || '/' || d.name || ' is registered in it'
         FROM additions_sets s JOIN dats d ON d.id = s.dat_id
         WHERE s.root = ?1 AND s.unregistered_at IS NULL ORDER BY 1",
        "SELECT 'it is registered as an ingest folder'
         FROM ingest_folders WHERE root = ?1 AND unregistered_at IS NULL",
    ] {
        holds.extend(strings(c, sql, [name])?);
    }
    Ok(holds)
}

fn strings(c: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut stmt = c.prepare(sql)?;
    let rows = stmt.query_map(params, |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The Jobs running on Root `name`: reading or writing a folder in it, or measuring it.
fn running_on(c: &Connection, name: &str) -> Result<Vec<String>> {
    strings(
        c,
        "SELECT DISTINCT replace(j.kind, '_', ' ') || ' job ' || j.id || ' is running on it'
         FROM jobs j
         LEFT JOIN source_sets s ON s.id = j.source_set_id
         LEFT JOIN library_folders f ON f.id = j.library_folder_id
         LEFT JOIN source_sets fs ON fs.id = f.source_set_id
         LEFT JOIN additions_sets a ON a.id = j.additions_set_id
         LEFT JOIN ingest_folders i ON i.id = j.ingest_folder_id
         WHERE j.state = 'running'
           AND (j.kind = 'calibration' OR ?1 IN (s.root, f.root, fs.root, a.root, i.root))
         ORDER BY j.id",
        [name],
    )
}

/// What a piece of approved work writes to in a Root: the Root as a whole, which it must be writable for, or one
/// Source Set's folder, which `writable_paths` may let it write to.
enum Target {
    Whole,
    Set(String),
}

/// Approved work that writes to a Root, and how to void it if it hasn't started.
struct Work {
    what: String,
    target: Target,
    voidable: Option<VoidedPlan>,
}

/// Every piece of approved work not yet finished that writes to Root `name`.
fn writing_work(c: &Connection, name: &str) -> Result<Vec<Work>> {
    let mut work = Vec::new();
    // A Build writes to its Library Folder, and to its Source Set's folder when it places or returns Games.
    let builds = c
        .prepare(
            "SELECT j.id, j.state, j.plan_id, f.root, f.path, fs.root, fs.path, fs.name,
                    EXISTS (SELECT 1 FROM json_each(p.content, '$.items')
                            WHERE json_extract(value, '$.changes_source_set'))
                    OR COALESCE(json_array_length(p.content, '$.returns'), 0) > 0
             FROM jobs j JOIN library_folders f ON f.id = j.library_folder_id
             JOIN plans p ON p.id = j.plan_id
             LEFT JOIN source_sets fs ON fs.id = f.source_set_id
             WHERE j.kind = 'build' AND j.state IN ('queued', 'running') AND ?1 IN (f.root, fs.root)
             ORDER BY j.id",
        )?
        .query_map([name], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, bool>(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (job, state, plan, root, path, set_root, set_path, set, changes_set) in builds {
        let void = |writes_to: String| {
            (state == "queued").then_some(VoidedPlan {
                kind: VoidedPlanKind::Build,
                plan,
                job,
                writes_to,
            })
        };
        if root == name {
            let writes_to = format!("play library folder {root}/{path}");
            work.push(Work {
                what: format!("build job {job} writes to {writes_to}"),
                target: Target::Whole,
                voidable: void(writes_to),
            });
        }
        if let (true, Some(set_root), Some(set_path), Some(set)) =
            (changes_set, set_root, set_path, set)
            && set_root == name
        {
            let writes_to = format!("source set {set}");
            work.push(Work {
                what: format!("build job {job} moves games in {writes_to}"),
                target: Target::Set(set_path),
                voidable: void(writes_to),
            });
        }
    }
    // A Build stopped part way writes again when the service settles its folder.
    for what in strings(
        c,
        "SELECT 'build ' || b.id || ' of play library folder ' || f.root || '/' || f.path
                    || ' stopped before it finished, and is settled when the service next starts'
         FROM builds b JOIN library_folders f ON f.id = b.folder_id
         WHERE f.root = ?1
           AND (b.state = 'running' OR (b.state IN ('failed', 'interrupted') AND b.cleaned_at IS NULL))
           AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.id = b.job_id AND j.state IN ('queued', 'running'))",
        [name],
    )? {
        work.push(Work {
            what,
            target: Target::Whole,
            voidable: None,
        });
    }
    // A repair writes to its Source Set's folder; one whose Job hasn't started has changed nothing yet.
    let repairs = c
        .prepare(
            "SELECT p.id, s.name, s.path,
                    (SELECT j.id FROM jobs j WHERE j.repair_plan_id = p.id AND j.state = 'queued'
                     ORDER BY j.id DESC LIMIT 1),
                    NOT EXISTS (SELECT 1 FROM repair_steps st WHERE st.plan_id = p.id AND st.state <> 'pending'),
                    NOT EXISTS (SELECT 1 FROM jobs j WHERE j.repair_plan_id = p.id AND j.state = 'running')
             FROM repair_plans p JOIN source_sets s ON s.id = p.source_set_id
             WHERE s.root = ?1 AND p.approved_at IS NOT NULL AND p.finished_at IS NULL
             ORDER BY p.id",
        )?
        .query_map([name], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, bool>(4)?,
                r.get::<_, bool>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (plan, set, path, queued, untouched, not_running) in repairs {
        let writes_to = format!("source set {set}");
        let voidable = match (queued, untouched && not_running) {
            (Some(job), true) => Some(VoidedPlan {
                kind: VoidedPlanKind::Repair,
                plan,
                job,
                writes_to: writes_to.clone(),
            }),
            _ => None,
        };
        work.push(Work {
            what: format!("repair plan {plan} of {writes_to} is approved and not finished"),
            target: Target::Set(path),
            voidable,
        });
    }
    // Work that is carried out as it is approved, and finished when the service next starts if it was stopped.
    for (sql, set) in [
        (
            "SELECT 'deletion ' || d.id || ' of kept files of play library folder ' || f.root || '/' || f.path
                    || ' is not finished', f.path
             FROM kept_deletions d JOIN library_folders f ON f.id = d.folder_id
             WHERE f.root = ?1 AND d.approved_at IS NOT NULL AND d.finished_at IS NULL",
            false,
        ),
        (
            "SELECT 'the move of play library folder ' || root || '/' || from_path || ' to ' || to_path
                    || ' is not finished', to_path
             FROM library_folder_moves WHERE root = ?1 AND approved_at IS NOT NULL AND finished_at IS NULL",
            false,
        ),
        (
            "SELECT 'deletion ' || d.id || ' of kept files of source set ' || s.name || ' is not finished', s.path
             FROM source_kept_deletions d JOIN source_sets s ON s.id = d.source_set_id
             WHERE s.root = ?1 AND d.approved_at IS NOT NULL AND d.finished_at IS NULL",
            true,
        ),
        (
            "SELECT 'ingest plan ' || p.id || ' is approved and not finished', ''
             FROM ingest_plans p JOIN ingest_folders i ON i.id = p.folder_id
             WHERE i.root = ?1 AND p.approved_at IS NOT NULL AND p.finished_at IS NULL",
            false,
        ),
        (
            "SELECT DISTINCT 'a move of ' || name || ' from an ingest folder into ' || dest_rel
                    || ' is not finished', dest_rel
             FROM ingest_moves WHERE dest_root = ?1 AND dest_kind = 'path' AND state NOT IN ('done', 'failed')",
            false,
        ),
        (
            "SELECT 'the addition ' || a.name || ' is being changed', s.path
             FROM additions a JOIN additions_sets s ON s.id = a.set_id
             WHERE s.root = ?1 AND a.state IN ('pending', 'staging', 'retiring', 'restoring')",
            false,
        ),
        (
            "SELECT 'additions job ' || j.id || ' is queued', s.path
             FROM jobs j JOIN additions_sets s ON s.id = j.additions_set_id
             WHERE s.root = ?1 AND j.state IN ('queued', 'running')",
            false,
        ),
        (
            "SELECT 'deletion ' || d.id || ' of kept additions is not finished', s.path
             FROM additions_kept_deletions d JOIN additions_sets s ON s.id = d.additions_set_id
             WHERE s.root = ?1 AND d.approved_at IS NOT NULL AND d.finished_at IS NULL",
            false,
        ),
    ] {
        for (what, path) in pairs(c, sql, name)? {
            work.push(Work {
                what,
                target: if set { Target::Set(path) } else { Target::Whole },
                voidable: None,
            });
        }
    }
    Ok(work)
}

fn pairs(c: &Connection, sql: &str, name: &str) -> Result<Vec<(String, String)>> {
    Ok(c.prepare(sql)?
        .query_map([name], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Why voided work's Job ended.
fn void_reason(root: &str) -> String {
    format!(
        "voided: root {root} was made less writable after this plan was approved, so it can no longer write where \
         it would have; nothing was changed"
    )
}

/// Ends voidable `plan`'s queued Job, and finishes a repair plan with every step failed, in `tx`.
fn void(tx: &Transaction, plan: &VoidedPlan, root: &str) -> Result<()> {
    let why = void_reason(root);
    let at = now();
    let ended = tx.execute(
        "UPDATE jobs SET state = 'failed', error = ?2, finished_at = ?3, version = version + 1
         WHERE id = ?1 AND state = 'queued'",
        params![plan.job, why, at],
    )?;
    if ended != 1 {
        return Err(Error::Conflict(format!(
            "job {} started while root {root} was being changed; nothing was changed",
            plan.job
        )));
    }
    if plan.kind == VoidedPlanKind::Repair {
        Store::void_repair(tx, plan.plan, &why, &at)?;
    }
    Ok(())
}

/// A refusal naming what stands in the way.
fn conflict<T>(name: &str, change: &str, holds: &[String]) -> Result<T> {
    Err(Error::Conflict(format!(
        "Root {name}'s {change} can't change yet: {}. Try again once that has finished.",
        holds.join("; ")
    )))
}

impl Store {
    /// Every registered Root, as it is now. Cheap: a snapshot the last change made.
    pub fn roots(&self) -> Arc<Roots> {
        self.roots.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Every registered Root, by name, as the API shows it.
    pub fn roots_view(&self) -> Vec<title_select_api::Root> {
        self.roots().iter().map(Root::view).collect()
    }

    /// Every recorded change to the Root named `name`, oldest first: who made it, when, and what it left.
    pub fn root_changes(&self, name: &str) -> Result<Vec<RootChange>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT action, root_after, changed_by, changed_at FROM root_changes WHERE root = ?1 ORDER BY id",
            )?;
            let rows = stmt.query_map([name], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            let mut changes = Vec::new();
            for row in rows {
                let (action, after, changed_by, changed_at) = row?;
                changes.push(RootChange {
                    action,
                    after: after.and_then(|a| serde_json::from_str(&a).ok()),
                    changed_by,
                    changed_at,
                });
            }
            Ok(changes)
        })
    }

    /// Reads the Roots afresh into the snapshot, and works the Ceilings out again for them.
    pub(crate) fn reload_roots(&self) -> Result<()> {
        let roots = Arc::new(self.read(load)?);
        *self.roots.write().unwrap_or_else(|e| e.into_inner()) = roots;
        Ok(())
    }

    fn roots_changed(&self) -> Result<()> {
        self.reload_roots()?;
        self.refresh_ceilings()?;
        Ok(())
    }

    /// Registers the Root `spec` gives, as `actor`.
    pub fn register_root(&self, spec: &RootSpec, actor: &str) -> Result<Root> {
        let root = checked(spec)?;
        check_folder(&root)?;
        let _lifecycle = self.lifecycle_lock();
        let saved = self.write(|tx| {
            if stored(tx, &root.name)?.is_some() {
                return refused(format!(
                    "A root named {} is registered already; choose another name, or edit that one.",
                    root.name
                ));
            }
            refuse_second_additions(tx, &root)?;
            save(tx, &root, "registered", actor)
        })?;
        self.roots_changed()?;
        Ok(saved)
    }

    /// Replaces Root `name` with what `spec` gives, as `actor`. Its name can't change. Making it less writable voids
    /// each approved plan, not yet started, that writes where it may no longer write: those are returned.
    pub fn edit_root(&self, name: &str, spec: &RootSpec, actor: &str) -> Result<RootEdited> {
        if spec.name != name {
            return refused(format!(
                "A root's name can't be changed: records of the folders in root {name} refer to it by name. \
                 Register a new root instead."
            ));
        }
        let root = checked(spec)?;
        let _lifecycle = self.lifecycle_lock();
        let was = self
            .read(|c| stored(c, name))?
            .ok_or_else(|| Error::NotFound(format!("There is no root {name}.")))?;
        let path_changed = was.path != root.path;
        let writability_changed =
            was.writable != root.writable || was.writable_paths != root.writable_paths;
        if path_changed || writability_changed {
            check_folder(&root)?;
        }
        let (saved, voided) = self.write(|tx| {
            if was.kind != root.kind {
                let holds = registrations_in(tx, name)?;
                if !holds.is_empty() {
                    return conflict(name, "kind", &holds);
                }
            }
            refuse_second_additions(tx, &root)?;
            if root.writable && !was.writable {
                let sets = strings(
                    tx,
                    "SELECT name FROM source_sets WHERE root = ?1 AND unregistered_at IS NULL ORDER BY name",
                    [name],
                )?;
                if !sets.is_empty() {
                    return refused(format!(
                        "Root {name} can't be made writable while it holds source sets ({}): Title Select writes to \
                         a source set's folder only where you list it in writable paths.",
                        sets.join(", ")
                    ));
                }
            }
            let mut voided = Vec::new();
            if path_changed || writability_changed {
                let running = running_on(tx, name)?;
                if !running.is_empty() {
                    let change = if path_changed { "path" } else { "writability" };
                    return conflict(name, change, &running);
                }
                let work = writing_work(tx, name)?;
                if path_changed && !work.is_empty() {
                    let holds: Vec<String> = work.into_iter().map(|w| w.what).collect();
                    return conflict(name, "path", &holds);
                }
                let after = Root {
                    writable: root.writable,
                    writable_paths: root.writable_paths.clone(),
                    ..was.clone()
                };
                let lost: Vec<Work> = work
                    .into_iter()
                    .filter(|w| was.allows(&w.target) && !after.allows(&w.target))
                    .collect();
                let unvoidable: Vec<String> = lost
                    .iter()
                    .filter(|w| w.voidable.is_none())
                    .map(|w| w.what.clone())
                    .collect();
                if !unvoidable.is_empty() {
                    return conflict(name, "writability", &unvoidable);
                }
                let mut seen = BTreeSet::new();
                for plan in lost.into_iter().filter_map(|w| w.voidable) {
                    if seen.insert((plan.kind == VoidedPlanKind::Repair, plan.plan)) {
                        void(tx, &plan, name)?;
                        voided.push(plan);
                    }
                }
            }
            Ok((save(tx, &root, "edited", actor)?, voided))
        })?;
        self.roots_changed()?;
        Ok(RootEdited {
            root: saved.view(),
            voided,
        })
    }

    /// Unregisters Root `name`, as `actor`: refused, naming what, while anything is registered in it. No file is
    /// touched.
    pub fn unregister_root(&self, name: &str, actor: &str) -> Result<()> {
        let _lifecycle = self.lifecycle_lock();
        self.write(|tx| {
            if stored(tx, name)?.is_none() {
                return Err(Error::NotFound(format!("There is no root {name}.")));
            }
            let holds = registrations_in(tx, name)?;
            if !holds.is_empty() {
                return Err(Error::Conflict(format!(
                    "Root {name} can't be unregistered while anything is registered in it: {}. Unregister those \
                     first.",
                    holds.join("; ")
                )));
            }
            tx.execute("DELETE FROM roots WHERE name = ?1", [name])?;
            record(tx, name, "unregistered", None, actor, &now())
        })?;
        self.roots_changed()
    }

    /// Sets the Operator's Ceilings of each Root `overrides` names, as `actor`, recording each change. A Root not
    /// named keeps its own.
    pub(crate) fn set_root_ceilings(
        tx: &Transaction,
        overrides: &BTreeMap<String, RootOverrides>,
        actor: &str,
    ) -> Result<()> {
        for (name, ceilings) in overrides {
            let Some(was) = stored(tx, name)? else {
                return Err(Error::Refused(format!("There is no root {name}.")));
            };
            if was.overrides() == *ceilings {
                continue;
            }
            let root = Checked {
                name: was.name.clone(),
                path: was.path.clone(),
                kind: was.kind,
                writable: was.writable,
                writable_paths: was.writable_paths.clone(),
                overrides: ceilings.clone(),
            };
            save(tx, &root, "edited", actor)?;
        }
        Ok(())
    }
}
