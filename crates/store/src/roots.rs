//! Roots: the folders Stage Select may read, registered, edited and unregistered by the Operator in the store
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
use stage_select_api::JobState;
use stage_select_api::paths::relative_path;
use stage_select_api::repair::{RepairPlanContent, RepairStep};
use stage_select_api::roots::{
    Folder, FolderListing, FolderSuggestion, FolderSuggestions, RootChange, RootEdited, RootSpec,
    VoidedPlan, VoidedPlanKind,
};
use stage_select_api::settings::{MAX_MIB_PER_S, MAX_WORKERS, RootOverrides};

pub use stage_select_api::roots::RootKind;

use crate::{Error, Result, Store, now};

/// The common mount points whose folders the workbench suggests registering as Roots.
pub const MOUNT_POINTS: [&str; 4] = ["/data", "/mnt", "/media", "/srv"];
/// The most entries of a mount point read to suggest the folders in it.
pub const READ_PER_MOUNT: usize = 1000;
/// The most folders in a mount point suggested.
pub const SUGGESTED_PER_MOUNT: usize = 50;
/// The most entries of a folder read to list the folders in it.
pub const READ_PER_FOLDER: usize = 10_000;
/// The most folders in a folder listed.
pub const FOLDERS_LISTED: usize = 500;

/// A registered Root, as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub name: String,
    pub path: PathBuf,
    pub kind: RootKind,
    /// Whether Stage Select may write to this Root: Builds to its Library Folders, and repairs to any Source Set in it.
    /// A Source Set is registered only in a read-only Root.
    pub writable: bool,
    /// For a read-only Root: the paths, relative to it, of the Source Sets in it that Stage Select may change, by
    /// Repairs and by Single Copy placing their Games. Each Source Set switches its own path in or out.
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

    /// Whether Stage Select may write to the folder `rel` of this Root: the Root is writable, or lists `rel` among
    /// its `writable_paths`.
    pub fn may_write(&self, rel: &str) -> bool {
        self.writable || relative_path(rel).is_ok_and(|rel| self.writable_paths.contains(&rel))
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
    pub fn view(&self) -> stage_select_api::Root {
        stage_select_api::Root {
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

    /// What it would let Stage Select write to, as `target` needs.
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
        rows.into_iter()
            .map(with_paths)
            .collect::<Result<Vec<_>>>()?,
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

/// Checks that `root`'s folder is there, and that Stage Select can write to it if it is to.
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
            "Root {name} is to be writable, but Stage Select can't write to {path}: {e}."
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
    save_creating(tx, root, action, actor, None)
}

/// [`save`], recording `created` as the folder the change created.
fn save_creating(
    tx: &Transaction,
    root: &Checked,
    action: &str,
    actor: &str,
    created: Option<&Path>,
) -> Result<Root> {
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
    if let Some(created) = created {
        tx.execute(
            "UPDATE root_changes SET created_folder = ?1 WHERE id = last_insert_rowid()",
            [created.display().to_string()],
        )?;
    }
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
fn registrations_in(c: &Connection, name: &str) -> Result<Vec<String>> {
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

/// The Kept Files in Root `name` that Stage Select still manages though their registration is Unregistered: its
/// marker stays so that you can delete them, which needs the Root.
fn managed_kept_in(c: &Connection, name: &str) -> Result<Vec<String>> {
    let mut holds = Vec::new();
    for sql in [
        "SELECT count(*) || ' kept files of unregistered source set ' || s.name || ' are still managed in it'
         FROM source_kept k JOIN source_sets s ON s.id = k.source_set_id
         WHERE s.root = ?1 AND s.unregistered_at IS NOT NULL AND k.state IN ('kept', 'deleting')
         GROUP BY s.id ORDER BY s.name",
        "SELECT count(*) || ' kept files of unregistered play library folder ' || f.root || '/' || f.path
                || ' are still managed in it'
         FROM kept_files k JOIN library_folders f ON f.id = k.folder_id
         WHERE f.root = ?1 AND f.unregistered_at IS NOT NULL AND k.directory = 0
           AND k.state IN ('keeping', 'kept', 'deleting', 'quarantined', 'restoring')
         GROUP BY f.id ORDER BY f.path",
        "SELECT count(*) || ' kept additions of the unregistered additions set of ' || d.provider || '/' || d.name
                || ' are still managed in it'
         FROM additions_kept k JOIN additions_sets s ON s.id = k.additions_set_id JOIN dats d ON d.id = s.dat_id
         WHERE s.root = ?1 AND s.unregistered_at IS NOT NULL AND k.state IN ('keeping', 'kept', 'deleting')
         GROUP BY s.id ORDER BY 1",
    ] {
        holds.extend(strings(c, sql, [name])?);
    }
    Ok(holds)
}

/// What holds Root `name` from being unregistered: each registration in it, each Kept File it still holds for you to
/// delete, and every piece of work not yet finished that reads or writes in it.
pub(crate) fn holds(c: &Connection, name: &str) -> Result<Vec<String>> {
    let mut holds = registrations_in(c, name)?;
    holds.extend(managed_kept_in(c, name)?);
    holds.extend(dependents(c, name)?.into_iter().map(|d| d.what));
    let mut seen = BTreeSet::new();
    holds.retain(|h| seen.insert(h.clone()));
    Ok(holds)
}

fn strings(c: &Connection, sql: &str, params: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut stmt = c.prepare(sql)?;
    let rows = stmt.query_map(params, |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// What a piece of work writes to in a Root: the Root as a whole, which it must be writable for, or one Source Set's
/// folder, which `writable_paths` may let it write to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Whole,
    Set(String),
}

/// A piece of work not yet finished that reads or writes in a Root.
struct Dep {
    /// What it is, as a hold.
    what: String,
    /// What it writes to in the Root; none when it only reads there.
    writes: Option<Target>,
    /// Its Job, while that runs.
    running: Option<String>,
    /// How to void it, while it is approved and nothing of it has started.
    voidable: Option<VoidedPlan>,
}

/// The Library Folder `id`'s Root and path.
fn folder_at(c: &Connection, id: i64) -> Result<Option<(String, String)>> {
    Ok(c.query_row(
        "SELECT root, path FROM library_folders WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

type BuildRow = (
    i64,
    String,
    i64,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    Option<String>,
);

type RepairRow = (
    i64,
    String,
    String,
    String,
    String,
    Option<i64>,
    Option<i64>,
    bool,
);

/// Every piece of work not yet finished that reads or writes in Root `name`: each Job Queued or Running, by every
/// folder it reads or writes, and each approved plan, by every folder it changes, wherever its registration is and
/// whether or not that is still registered.
fn dependents(c: &Connection, name: &str) -> Result<Vec<Dep>> {
    let mut deps = Vec::new();
    let mut dep =
        |root: &str, what: String, writes: Option<Target>, running: Option<String>, voidable| {
            if root == name {
                deps.push(Dep {
                    what,
                    writes,
                    running,
                    voidable,
                });
            }
        };
    let every_root = strings(c, "SELECT name FROM roots ORDER BY name", [])?;
    // A Job reads its Source Set, with the Library Folders holding its Placed Games, its Library Folder or its
    // Ingest Folder; an Additions Job writes to its Additions Set; a Calibration measures every Root.
    let jobs = c
        .prepare(
            "SELECT j.id, replace(j.kind, '_', ' '), j.state, s.root, f.root, fs.root, a.root, i.root,
                    (SELECT group_concat(root, char(31)) FROM (
                         SELECT DISTINCT pf.root FROM placed_files p JOIN library_folders pf ON pf.id = p.folder_id
                         WHERE p.source_set_id = j.source_set_id))
             FROM jobs j
             LEFT JOIN source_sets s ON s.id = j.source_set_id
             LEFT JOIN library_folders f ON f.id = j.library_folder_id
             LEFT JOIN source_sets fs ON fs.id = f.source_set_id
             LEFT JOIN additions_sets a ON a.id = j.additions_set_id
             LEFT JOIN ingest_folders i ON i.id = j.ingest_folder_id
             WHERE j.state IN ('queued', 'running') AND j.kind NOT IN ('build', 'repair')
             ORDER BY j.id",
        )?
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                [
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ],
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (job, kind, state, read, additions, ingest, placed) in jobs {
        let running = (state == "running").then(|| format!("{kind} job {job} is running on it"));
        // Root names hold no control character, so the unit separator joins them unambiguously.
        let placed = placed
            .iter()
            .flat_map(|p| p.split('\u{1f}').map(str::to_owned));
        let mut reads: BTreeSet<String> = read
            .into_iter()
            .flatten()
            .chain(ingest)
            .chain(placed)
            .collect();
        if kind == "calibration" {
            reads.extend(every_root.iter().cloned());
        }
        if let Some(root) = &additions {
            dep(
                root,
                format!("{kind} job {job} is {state}"),
                Some(Target::Whole),
                running.clone(),
                None,
            );
            reads.remove(root);
        }
        for root in reads {
            dep(
                &root,
                format!("{kind} job {job} is {state} on it"),
                None,
                running.clone(),
                None,
            );
        }
    }
    // A Build writes to its Library Folder, and to its Source Set's folder when it places or returns Games; it reads
    // its Source Set and its system's additions.
    let builds = c
        .prepare(
            "SELECT j.id, j.state, j.plan_id, f.root, f.path, fs.root, fs.path, fs.name,
                    EXISTS (SELECT 1 FROM json_each(p.content, '$.items')
                            WHERE json_extract(value, '$.changes_source_set'))
                    OR COALESCE(json_array_length(p.content, '$.returns'), 0) > 0,
                    (SELECT a.root FROM additions_sets a WHERE a.dat_id = fs.dat_id AND a.unregistered_at IS NULL)
             FROM jobs j JOIN library_folders f ON f.id = j.library_folder_id
             JOIN plans p ON p.id = j.plan_id
             LEFT JOIN source_sets fs ON fs.id = f.source_set_id
             WHERE j.kind = 'build' AND j.state IN ('queued', 'running')
             ORDER BY j.id",
        )?
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<BuildRow>>>()?;
    for (job, state, plan, root, path, set_root, set_path, set, changes_set, additions) in builds {
        let running = (state == "running").then(|| format!("build job {job} is running on it"));
        let void = |writes_to: String| {
            (state == "queued").then_some(VoidedPlan {
                kind: VoidedPlanKind::Build,
                plan,
                job,
                writes_to,
            })
        };
        let folder = format!("play library folder {root}/{path}");
        dep(
            &root,
            format!("build job {job} writes to {folder}"),
            Some(Target::Whole),
            running.clone(),
            void(folder),
        );
        if let (Some(set_root), Some(set_path), Some(set)) = (set_root, set_path, set) {
            let writes_to = format!("source set {set}");
            match changes_set {
                true => dep(
                    &set_root,
                    format!("build job {job} moves games in {writes_to}"),
                    Some(Target::Set(set_path)),
                    running.clone(),
                    void(writes_to),
                ),
                false => dep(
                    &set_root,
                    format!("build job {job} reads {writes_to}"),
                    None,
                    running.clone(),
                    None,
                ),
            }
        }
        if let Some(additions) = additions {
            dep(
                &additions,
                format!("build job {job} may read the additions in it"),
                None,
                running,
                None,
            );
        }
    }
    // A Build stopped part way writes again when the service settles its folder, and puts back what it placed.
    let stopped = c
        .prepare(
            "SELECT b.id, f.root, f.path, fs.root, fs.path, fs.name,
                    EXISTS (SELECT 1 FROM json_each(p.content, '$.items')
                            WHERE json_extract(value, '$.changes_source_set'))
                    OR COALESCE(json_array_length(p.content, '$.returns'), 0) > 0
             FROM builds b JOIN library_folders f ON f.id = b.folder_id
             JOIN plans p ON p.id = b.plan_id
             LEFT JOIN source_sets fs ON fs.id = f.source_set_id
             WHERE (b.state = 'running' OR (b.state IN ('failed', 'interrupted') AND b.cleaned_at IS NULL))
               AND NOT EXISTS (SELECT 1 FROM jobs j WHERE j.id = b.job_id AND j.state IN ('queued', 'running'))
             ORDER BY b.id",
        )?
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, bool>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (build, root, path, set_root, set_path, set, changes_set) in stopped {
        let what = format!(
            "build {build} of play library folder {root}/{path} stopped before it finished, and is settled when \
             the service next starts"
        );
        if let (true, Some(set_root), Some(set_path), Some(_)) =
            (changes_set, set_root, set_path, set)
        {
            dep(
                &set_root,
                what.clone(),
                Some(Target::Set(set_path)),
                None,
                None,
            );
        }
        dep(&root, what, Some(Target::Whole), None, None);
    }
    // A repair writes to its Source Set's folder, to each Library Folder holding Placed Games it changes, and to each
    // Ingest Folder it fills a game from. One whose Job hasn't started, and that has changed nothing yet, is voidable.
    let repairs = c
        .prepare(
            "SELECT p.id, p.content, s.name, s.root, s.path,
                    (SELECT j.id FROM jobs j WHERE j.repair_plan_id = p.id AND j.state = 'queued'
                     ORDER BY j.id DESC LIMIT 1),
                    (SELECT j.id FROM jobs j WHERE j.repair_plan_id = p.id AND j.state = 'running' LIMIT 1),
                    NOT EXISTS (SELECT 1 FROM repair_steps st WHERE st.plan_id = p.id
                                AND NOT (st.state = 'pending'
                                         OR (st.state = 'moving' AND EXISTS (
                                             SELECT 1 FROM ingest_moves m WHERE m.id = st.move_id
                                             AND m.state = 'pending'))))
             FROM repair_plans p JOIN source_sets s ON s.id = p.source_set_id
             WHERE p.approved_at IS NOT NULL AND p.finished_at IS NULL
             ORDER BY p.id",
        )?
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<RepairRow>>>()?;
    for (plan, content, set, root, path, queued, running, untouched) in repairs {
        let content: RepairPlanContent = serde_json::from_str(&content)
            .map_err(|e| Error::Io(format!("repair plan {plan} does not read: {e}")))?;
        let running = running.map(|job| format!("repair job {job} is running on it"));
        let void = |writes_to: String| match (queued, untouched && running.is_none()) {
            (Some(job), true) => Some(VoidedPlan {
                kind: VoidedPlanKind::Repair,
                plan,
                job,
                writes_to,
            }),
            _ => None,
        };
        let of = format!("repair plan {plan} of source set {set}");
        dep(
            &root,
            format!("{of} is approved and not finished"),
            Some(Target::Set(path)),
            running.clone(),
            void(format!("source set {set}")),
        );
        for folder in crate::repairs::placed_folders(&content) {
            if let Some((root, path)) = folder_at(c, folder)? {
                let folder = format!("play library folder {root}/{path}");
                dep(
                    &root,
                    format!("{of} changes placed games in {folder}"),
                    Some(Target::Whole),
                    running.clone(),
                    void(folder),
                );
            }
        }
        let ingest: BTreeSet<&str> = content
            .steps
            .iter()
            .filter_map(|s| match &s.step {
                RepairStep::Fill { ingest_root, .. } => Some(ingest_root.as_str()),
                _ => None,
            })
            .collect();
        for root in ingest {
            dep(
                root,
                format!("{of} moves files out of ingest folder {root}"),
                Some(Target::Whole),
                running.clone(),
                void(format!("ingest folder {root}")),
            );
        }
    }
    // A move out of an Ingest Folder, not a repair's, writes to that folder and to where it goes: a Source Set's
    // folder, which `writable_paths` may let it write to, or another folder of the Root it goes to.
    let moves = c
        .prepare(
            "SELECT m.name, i.root, m.dest_root, m.dest_rel,
                    EXISTS (SELECT 1 FROM source_sets s WHERE s.root = m.dest_root AND s.path = m.dest_rel)
             FROM ingest_moves m JOIN ingest_folders i ON i.id = m.folder_id
             WHERE m.state NOT IN ('done', 'failed')
               AND NOT EXISTS (SELECT 1 FROM repair_steps st JOIN repair_plans p ON p.id = st.plan_id
                               WHERE st.move_id = m.id AND p.finished_at IS NULL)
             ORDER BY m.id",
        )?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, bool>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (file, from, dest_root, dest_rel, into_set) in moves {
        dep(
            &from,
            format!("a move of {file} out of ingest folder {from} is not finished"),
            Some(Target::Whole),
            None,
            None,
        );
        if let (Some(dest_root), Some(dest_rel)) = (dest_root, dest_rel) {
            let target = match into_set {
                true => Target::Set(dest_rel.clone()),
                false => Target::Whole,
            };
            dep(
                &dest_root,
                format!(
                    "a move of {file} from ingest folder {from} into {dest_rel} is not finished"
                ),
                Some(target),
                None,
                None,
            );
        }
    }
    // Work carried out as it is approved, and finished when the service next starts if it was stopped; and what an
    // Addition being added reads.
    for (sql, set, writes) in [
        (
            "SELECT f.root, 'deletion ' || d.id || ' of kept files of play library folder ' || f.root || '/' || f.path
                    || ' is not finished', f.path
             FROM kept_deletions d JOIN library_folders f ON f.id = d.folder_id
             WHERE d.approved_at IS NOT NULL AND d.finished_at IS NULL",
            false,
            true,
        ),
        (
            "SELECT f.root, 'the kept file ' || k.name || ' of play library folder ' || f.root || '/' || f.path
                    || ' is being ' || CASE k.state WHEN 'keeping' THEN 'kept' ELSE 'restored' END, f.path
             FROM kept_files k JOIN library_folders f ON f.id = k.folder_id
             WHERE k.state IN ('keeping', 'restoring')",
            false,
            true,
        ),
        (
            "SELECT root, 'the move of play library folder ' || root || '/' || from_path || ' to ' || to_path
                    || ' is not finished', to_path
             FROM library_folder_moves WHERE approved_at IS NOT NULL AND finished_at IS NULL",
            false,
            true,
        ),
        (
            "SELECT s.root, 'deletion ' || d.id || ' of kept files of source set ' || s.name || ' is not finished',
                    s.path
             FROM source_kept_deletions d JOIN source_sets s ON s.id = d.source_set_id
             WHERE d.approved_at IS NOT NULL AND d.finished_at IS NULL",
            true,
            true,
        ),
        (
            "SELECT i.root, 'ingest plan ' || p.id || ' is approved and not finished', ''
             FROM ingest_plans p JOIN ingest_folders i ON i.id = p.folder_id
             WHERE p.approved_at IS NOT NULL AND p.finished_at IS NULL",
            false,
            true,
        ),
        (
            "SELECT s.root, 'the addition ' || a.name || ' is being changed', s.path
             FROM additions a JOIN additions_sets s ON s.id = a.set_id
             WHERE a.state IN ('pending', 'staging', 'retiring', 'restoring')",
            false,
            true,
        ),
        (
            "SELECT s.root, 'deletion ' || d.id || ' of kept additions is not finished', s.path
             FROM additions_kept_deletions d JOIN additions_sets s ON s.id = d.additions_set_id
             WHERE d.approved_at IS NOT NULL AND d.finished_at IS NULL",
            false,
            true,
        ),
        (
            "SELECT s.root, 'the addition ' || a.name || ' is being added from a kept file of source set ' || s.name,
                    s.path,
                    (SELECT 'additions job ' || j.id || ' is running on it' FROM jobs j
                     WHERE j.kind = 'additions' AND j.state = 'running' AND j.additions_set_id = a.set_id LIMIT 1)
             FROM additions a JOIN repair_plans p ON p.id = a.repair_plan_id
             JOIN source_sets s ON s.id = p.source_set_id
             WHERE a.state IN ('pending', 'staging')",
            true,
            false,
        ),
        (
            "SELECT f.root, 'the addition ' || a.name || ' is being added from a kept file of play library folder '
                    || f.root || '/' || f.path, f.path,
                    (SELECT 'additions job ' || j.id || ' is running on it' FROM jobs j
                     WHERE j.kind = 'additions' AND j.state = 'running' AND j.additions_set_id = a.set_id LIMIT 1)
             FROM additions a JOIN plans p ON p.id = a.cleanup_plan_id
             JOIN library_folders f ON f.id = p.folder_id
             WHERE a.state IN ('pending', 'staging')",
            false,
            false,
        ),
    ] {
        let rows = c
            .prepare(sql)?
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    // The Job running on it, from the queries that give one.
                    match r.as_ref().column_count() > 3 {
                        true => r.get::<_, Option<String>>(3)?,
                        false => None,
                    },
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (root, what, path, running) in rows {
            let target = match set {
                true => Target::Set(path),
                false => Target::Whole,
            };
            dep(&root, what, writes.then_some(target), running, None);
        }
    }
    Ok(deps)
}

/// Why voided work's Job ended.
fn void_reason(root: &str) -> String {
    format!(
        "voided: root {root} was made less writable after this plan was approved, so it can no longer write where \
         it would have; nothing was changed"
    )
}

/// Why voided work's Job ended, when it would have written to Source Set `set`, which Stage Select may no longer
/// change.
fn set_void_reason(set: &str) -> String {
    format!(
        "voided: you stopped Stage Select changing source set {set} after this plan was approved, so it can no \
         longer write where it would have; nothing was changed"
    )
}

/// Ends voidable `plan`'s queued Job as any Job ends, releasing what it held, saying `why`; and finishes a repair
/// plan with every step failed, and every move out of an Ingest Folder it recorded and had not started failed with
/// it, in `tx`. `changing` names what is being changed, should the Job have started.
fn void(tx: &Transaction, plan: &VoidedPlan, why: &str, changing: &str) -> Result<()> {
    if !crate::jobs::end_queued(tx, plan.job, JobState::Failed, why)? {
        return Err(Error::Conflict(format!(
            "job {} started while {changing} was being changed; nothing was changed",
            plan.job
        )));
    }
    if plan.kind == VoidedPlanKind::Repair {
        let at = now();
        tx.execute(
            "UPDATE ingest_moves SET state = 'failed', error = ?2, blocked = NULL, finished_at = ?3
             WHERE state = 'pending'
               AND id IN (SELECT move_id FROM repair_steps WHERE plan_id = ?1 AND state = 'moving')",
            params![plan.plan, why, at],
        )?;
        tx.execute(
            "UPDATE repair_steps SET state = 'failed', error = ?2, finished_at = ?3
             WHERE plan_id = ?1 AND state = 'moving'",
            params![plan.plan, why, at],
        )?;
        Store::void_repair(tx, plan.plan, why, &at)?;
    }
    Ok(())
}

/// Lets Stage Select change the Source Set `set`, at `rel` in the read-only Root `name`, or stops it, in `tx`, as
/// `actor`: lists `rel` among the Root's `writable_paths`, or takes it out. The rules of widening and narrowing a
/// Root's writable paths hold, for the work that writes to this folder alone: letting it is refused while unfinished
/// work writes to the folder, and stopping it voids each approved plan, not yet started, that writes there, which are
/// returned, and is refused while any other work writes there. In a writable Root, which holds no Source Set, it can
/// only be let.
pub(crate) fn let_change_set(
    tx: &Transaction,
    name: &str,
    rel: &str,
    set: &str,
    on: bool,
    actor: &str,
) -> Result<Vec<VoidedPlan>> {
    let was =
        stored(tx, name)?.ok_or_else(|| Error::NotFound(format!("There is no root {name}.")))?;
    if was.writable {
        return match on {
            true => Ok(Vec::new()),
            false => refused(format!(
                "Root {name} is writable, so Stage Select may change everything in it; make it read-only first."
            )),
        };
    }
    let rel = relative_path(rel).map_err(|e| Error::Refused(format!("{rel:?}: a path {e}.")))?;
    if was.writable_paths.contains(&rel) == on {
        return Ok(Vec::new());
    }
    let target = Target::Set(rel.clone());
    let writers: Vec<Dep> = dependents(tx, name)?
        .into_iter()
        .filter(|d| d.writes.as_ref() == Some(&target))
        .collect();
    let mut seen = BTreeSet::new();
    let mut held = |deps: &mut dyn Iterator<Item = String>| -> Vec<String> {
        deps.filter(|w| seen.insert(w.clone())).collect()
    };
    let unvoidable = match on {
        true => held(&mut writers.iter().map(|d| d.what.clone())),
        false => held(
            &mut writers
                .iter()
                .filter(|d| d.running.is_some() || d.voidable.is_none())
                .map(|d| d.running.clone().unwrap_or_else(|| d.what.clone())),
        ),
    };
    if !unvoidable.is_empty() {
        return Err(Error::Conflict(format!(
            "Whether Stage Select may change source set {set} can't change yet: {}. Try again once that has \
             finished.",
            unvoidable.join("; ")
        )));
    }
    let mut voided = Vec::new();
    let mut seen = BTreeSet::new();
    for plan in writers.into_iter().filter_map(|d| d.voidable) {
        if seen.insert((plan.kind == VoidedPlanKind::Repair, plan.plan)) {
            void(
                tx,
                &plan,
                &set_void_reason(set),
                &format!("source set {set}"),
            )?;
            voided.push(plan);
        }
    }
    let mut writable_paths = was.writable_paths.clone();
    match on {
        true => writable_paths.push(rel),
        false => writable_paths.retain(|p| *p != rel),
    }
    let root = Checked {
        name: was.name.clone(),
        path: was.path.clone(),
        kind: was.kind,
        writable: false,
        writable_paths,
        overrides: was.overrides(),
    };
    save(tx, &root, "edited", actor)?;
    Ok(voided)
}

/// Refuses moving Root `name` to `path` where a Source Set in it would come to nest with one in another Root.
fn refuse_nesting_moved(tx: &Transaction, roots: &Roots, name: &str, path: &Path) -> Result<()> {
    let sets: Vec<(String, String)> = {
        let mut stmt = tx.prepare(
            "SELECT name, path FROM source_sets WHERE root = ?1 AND unregistered_at IS NULL ORDER BY name",
        )?;
        stmt.query_map([name], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?
    };
    for (set, rel) in sets {
        let Some(would_be) = crate::beneath::resolved(path, &rel) else {
            continue;
        };
        if let Some(how) = crate::sets::nesting(tx, roots, &would_be, Some(name))? {
            return refused(format!(
                "Root {name} can't move to {}: there, source set {set} {how}, and source sets never nest.",
                path.display()
            ));
        }
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

    /// Takes the lifecycle lock for committing work checked against `seen`, a snapshot read before: refused while
    /// any Root's path, kind or writability is no longer as it was then. Held until the work is recorded, so no edit
    /// of a Root falls between the check and the record, and an edit that follows sees the work.
    pub(crate) fn lock_roots_unchanged(
        &self,
        seen: &Roots,
    ) -> Result<std::sync::MutexGuard<'_, ()>> {
        let held = self.lifecycle_lock();
        let places = |roots: &Roots| -> Vec<_> {
            roots
                .iter()
                .map(|r| {
                    (
                        r.name.clone(),
                        r.path.clone(),
                        r.kind,
                        r.writable,
                        r.writable_paths.clone(),
                    )
                })
                .collect()
        };
        if places(&self.roots()) != places(seen) {
            return Err(Error::Conflict(
                "the roots changed while this was being approved; nothing was changed. Review it again".into(),
            ));
        }
        Ok(held)
    }

    /// The folders to suggest registering as Roots: each of `mount_points` that is a folder, then the folders
    /// directly in it, by name, but for those already registered. The listing is shallow and bounded, as a mount
    /// point may be a large or slow share: it reads at most [`READ_PER_MOUNT`] entries of each and suggests at most
    /// [`SUGGESTED_PER_MOUNT`] folders in it, and says when it stopped short. It never follows a symlink, so it never
    /// suggests a folder outside the mount points, and skips hidden folders and `lost+found`.
    pub fn folder_suggestions(&self, mount_points: &[PathBuf]) -> FolderSuggestions {
        let registered: Vec<PathBuf> = self.roots().iter().map(|r| r.path.clone()).collect();
        let mut out = FolderSuggestions::default();
        let mut suggest = |path: PathBuf| {
            if !registered.contains(&path)
                && let Some(text) = path.to_str()
            {
                out.folders.push(FolderSuggestion {
                    path: text.to_owned(),
                    writable: rustix::fs::access(&path, rustix::fs::Access::WRITE_OK).is_ok(),
                });
            }
        };
        for base in mount_points {
            if !std::fs::symlink_metadata(base).is_ok_and(|m| m.is_dir()) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(base) else {
                suggest(base.clone());
                continue;
            };
            let mut names = Vec::new();
            for (read, entry) in entries.enumerate() {
                if read == READ_PER_MOUNT {
                    out.more = true;
                    break;
                }
                let Ok(entry) = entry else { continue };
                let name = entry.file_name();
                let shown = name
                    .to_str()
                    .is_some_and(|n| !n.starts_with('.') && n != "lost+found");
                if shown && entry.file_type().is_ok_and(|t| t.is_dir()) {
                    names.push(name);
                }
            }
            names.sort();
            if names.len() > SUGGESTED_PER_MOUNT {
                names.truncate(SUGGESTED_PER_MOUNT);
                out.more = true;
            }
            suggest(base.clone());
            for name in names {
                suggest(base.join(name));
            }
        }
        out
    }

    /// The folders in the folder at the absolute `path`, to browse for a path: any folder the Service can see. It reads
    /// names only, never a file. Each folder is listed by name, case-insensitively, with whether the Service can write
    /// to it and the deepest Root it resolves into, following links. A symlink to a folder is listed as a link at its
    /// own path; any other file is left out, as are hidden folders (a leading dot, and `lost+found`) unless `hidden`.
    /// The listing is bounded, as a folder may be a large or slow share: it reads at most [`READ_PER_FOLDER`] entries
    /// and lists at most [`FOLDERS_LISTED`] folders, and says when it stopped short. A relative path, a `..` step, and
    /// a path that isn't a folder are refused.
    pub fn folders(&self, path: &str, hidden: bool) -> Result<FolderListing> {
        if !path.starts_with('/') {
            return Err(Error::Refused(format!(
                "the path must be absolute, starting with /: {path:?}"
            )));
        }
        let mut at = PathBuf::new();
        for part in Path::new(path).components() {
            match part {
                std::path::Component::ParentDir => {
                    return Err(Error::Refused(format!(
                        "the path can't step up with ..: {path}"
                    )));
                }
                std::path::Component::CurDir => {}
                part => at.push(part),
            }
        }
        let shown = at.display().to_string();
        match std::fs::metadata(&at) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(Error::Refused(format!("{shown} is not a folder"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::NotFound(format!(
                    "there is no folder at {shown} yet"
                )));
            }
            Err(_) => return Err(Error::Refused(format!("there is no folder at {shown}"))),
        }
        let entries = std::fs::read_dir(&at)
            .map_err(|e| Error::Refused(format!("the service can't read {shown}: {e}")))?;
        // Membership follows links, as the Host would (see `beneath`): each Root and folder is compared canonical.
        let roots: Vec<(PathBuf, String)> = self
            .roots()
            .iter()
            .filter_map(|r| Some((r.path.canonicalize().ok()?, r.name.clone())))
            .collect();
        let root_of = |folder: &Path| {
            let folder = folder.canonicalize().ok()?;
            roots
                .iter()
                .filter(|(path, _)| folder.starts_with(path))
                .max_by_key(|(path, _)| path.components().count())
                .map(|(_, name)| name.clone())
        };
        let mut out = FolderListing {
            path: shown,
            parent: at.parent().map(|p| p.display().to_string()),
            ..FolderListing::default()
        };
        for (read, entry) in entries.enumerate() {
            if read == READ_PER_FOLDER {
                out.more = true;
                break;
            }
            let Ok(entry) = entry else { continue };
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !hidden && (name.starts_with('.') || name == "lost+found") {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let folder = entry.path();
            let link = kind.is_symlink();
            if !(kind.is_dir() || link && std::fs::metadata(&folder).is_ok_and(|m| m.is_dir())) {
                continue;
            }
            let Some(text) = folder.to_str() else {
                continue;
            };
            out.folders.push(Folder {
                writable: rustix::fs::access(&folder, rustix::fs::Access::WRITE_OK).is_ok(),
                root: root_of(&folder),
                path: text.to_owned(),
                name,
                link,
            });
        }
        out.folders.sort_by(|a, b| {
            (a.name.to_lowercase(), &a.name).cmp(&(b.name.to_lowercase(), &b.name))
        });
        if out.folders.len() > FOLDERS_LISTED {
            out.folders.truncate(FOLDERS_LISTED);
            out.more = true;
        }
        Ok(out)
    }

    /// Every registered Root, by name, as the API shows it.
    pub fn roots_view(&self) -> Vec<stage_select_api::Root> {
        self.roots().iter().map(Root::view).collect()
    }

    /// Every recorded change to the Root named `name`, oldest first: who made it, when, and what it left.
    pub fn root_changes(&self, name: &str) -> Result<Vec<RootChange>> {
        self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT action, root_after, changed_by, changed_at, created_folder FROM root_changes
                 WHERE root = ?1 ORDER BY id",
            )?;
            let rows = stmt.query_map([name], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })?;
            let mut changes = Vec::new();
            for row in rows {
                let (action, after, changed_by, changed_at, created_folder) = row?;
                changes.push(RootChange {
                    action,
                    after: after.and_then(|a| serde_json::from_str(&a).ok()),
                    changed_by,
                    changed_at,
                    created_folder,
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

    pub(crate) fn roots_changed(&self) -> Result<()> {
        self.reload_roots()?;
        self.refresh_ceilings()?;
        Ok(())
    }

    /// Registers the Root `spec` gives, as `actor`. If `spec` says to create its folder and the folder is missing,
    /// creates it, and its missing parents, once the rest of the registration checks out.
    pub fn register_root(&self, spec: &RootSpec, actor: &str) -> Result<Root> {
        let root = checked(spec)?;
        let unclaimed = |c: &Connection| {
            if stored(c, &root.name)?.is_some() {
                return refused(format!(
                    "A root named {} is registered already; choose another name, or edit that one.",
                    root.name
                ));
            }
            refuse_second_additions(c, &root)
        };
        let _lifecycle = self.lifecycle_lock();
        // Dropped on a refusal or failure from here on, which removes what it made.
        let made = if spec.create && std::fs::symlink_metadata(&root.path).is_err() {
            self.read(|c| unclaimed(c))?;
            self.fault_point(crate::Fault::CreatingFolder);
            let made =
                crate::made_folders::create_folder(&root.path, &format!("Root {}", root.name))?;
            self.fault_point(crate::Fault::FolderCreated);
            Some(made)
        } else {
            None
        };
        let created = made
            .as_ref()
            .filter(|m| !m.is_empty())
            .map(|_| root.path.as_path());
        check_folder(&root)?;
        let saved = self.write(|tx| {
            unclaimed(tx)?;
            save_creating(tx, &root, "registered", actor, created)
        })?;
        if let Some(made) = made {
            made.keep();
        }
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
        self.fault_point(crate::Fault::RootEditing);
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
                // The same holds as unregistering: a kind change strands whatever is registered, managed or under
                // way in it just as removing it would.
                let holds = holds(tx, name)?;
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
                        "Root {name} can't be made writable while it holds source sets ({}): Stage Select changes \
                         a source set only where you let it, on the source set.",
                        sets.join(", ")
                    ));
                }
            }
            if path_changed {
                refuse_nesting_moved(tx, &self.roots(), name, &root.path)?;
            }
            let mut voided = Vec::new();
            if path_changed || writability_changed {
                let deps = dependents(tx, name)?;
                let change = if path_changed { "path" } else { "writability" };
                let mut seen = BTreeSet::new();
                let running: Vec<String> = deps
                    .iter()
                    .filter_map(|d| d.running.clone())
                    .filter(|r| seen.insert(r.clone()))
                    .collect();
                if !running.is_empty() {
                    return conflict(name, change, &running);
                }
                let writers: Vec<&Dep> = deps.iter().filter(|d| d.writes.is_some()).collect();
                // Compared by what each lets Stage Select write, not by its fields: listing paths on a Root that
                // was wholly writable narrows it. A change can widen and narrow at once (swapping one writable
                // path for another). While any writer depends on the Root, a change that widens at all is
                // refused, as a path change is; only a change that solely narrows voids the writers it shuts out.
                let widened = !was.writable
                    && (root.writable
                        || root.writable_paths.iter().any(|p| !was.writable_paths.contains(p)));
                if (path_changed || widened) && !writers.is_empty() {
                    let mut seen = BTreeSet::new();
                    let holds: Vec<String> = writers
                        .iter()
                        .map(|d| d.what.clone())
                        .filter(|w| seen.insert(w.clone()))
                        .collect();
                    return conflict(name, change, &holds);
                }
                let after = Root {
                    writable: root.writable,
                    writable_paths: root.writable_paths.clone(),
                    ..was.clone()
                };
                let lost: Vec<&Dep> = writers
                    .into_iter()
                    .filter(|d| {
                        let target = d.writes.as_ref().expect("a writer");
                        was.allows(target) && !after.allows(target)
                    })
                    .collect();
                let unvoidable: Vec<String> = lost
                    .iter()
                    .filter(|d| d.voidable.is_none())
                    .map(|d| d.what.clone())
                    .collect();
                if !unvoidable.is_empty() {
                    return conflict(name, change, &unvoidable);
                }
                let mut seen = BTreeSet::new();
                for plan in lost.into_iter().filter_map(|d| d.voidable.clone()) {
                    if seen.insert((plan.kind == VoidedPlanKind::Repair, plan.plan)) {
                        void(tx, &plan, &void_reason(name), &format!("root {name}"))?;
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

    /// Unregisters Root `name`, as `actor`: refused, naming what, while anything is registered in it, it holds Kept
    /// Files still managed, or work not yet finished reads or writes in it. No file is touched.
    pub fn unregister_root(&self, name: &str, actor: &str) -> Result<()> {
        let _lifecycle = self.lifecycle_lock();
        self.write(|tx| {
            if stored(tx, name)?.is_none() {
                return Err(Error::NotFound(format!("There is no root {name}.")));
            }
            let holds = holds(tx, name)?;
            if !holds.is_empty() {
                return Err(Error::Conflict(format!(
                    "Root {name} can't be unregistered while anything is registered, managed or under way in it: \
                     {}. Unregister or finish those first.",
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
