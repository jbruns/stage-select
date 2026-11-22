//! The Source Set registry: each Source Set is a Root plus a relative path, bound to a DAT.

use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::dats::split_dat;
use super::{Error, Result, Store, now};
use title_select_api::paths::{PathTemplate, TokenValues, relative_path};

pub use title_select_api::{LastVerified, SourceSetRecord, SourceSetStale};

pub(super) struct SourceSetRow {
    pub id: i64,
    pub dat_id: i64,
    pub record: SourceSetRecord,
}

const SET_QUERY: &str =
    "SELECT s.id, s.dat_id, s.name, s.root, s.path, d.provider || '/' || d.name, s.added_at,
            r.sha256, r.version, a.finished_at, s.follow,
            w.id, w.found_at, w.changed, w.added, w.removed,
            n.sha256, n.version, n.imported_at, s.coverage
     FROM source_sets s
     JOIN dats d ON d.id = s.dat_id
     LEFT JOIN audits a ON a.id = (SELECT id FROM audits
                                  WHERE source_set_id = s.id AND status = 'finished'
                                  ORDER BY finish_seq DESC LIMIT 1)
     LEFT JOIN dat_revisions r ON r.id = a.dat_revision_id
     LEFT JOIN stat_walks w ON w.id = (SELECT id FROM stat_walks WHERE source_set_id = s.id ORDER BY id DESC LIMIT 1)
                           AND w.audit_id = a.id AND w.changed + w.added + w.removed > 0
     LEFT JOIN dat_revisions n ON s.follow AND a.id IS NOT NULL
                               AND n.id = (SELECT id FROM dat_revisions
                                           WHERE dat_id = s.dat_id AND deleted_at IS NULL
                                           ORDER BY header_order DESC LIMIT 1)
                               AND n.header_order > r.header_order";

/// Each registered Source Set that is Stale, by name, with why.
pub(crate) fn stale_sets(c: &Connection) -> Result<Vec<(String, Vec<SourceSetStale>)>> {
    let mut stmt = c.prepare(&format!("{SET_QUERY} WHERE s.unregistered_at IS NULL"))?;
    let rows = stmt.query_map([], |r| set_from_row(r).map(|s| s.record))?;
    let mut stale = Vec::new();
    for set in rows {
        let set = set?;
        if !set.stale.is_empty() {
            stale.push((set.name, set.stale));
        }
    }
    Ok(stale)
}

fn set_from_row(row: &Row) -> rusqlite::Result<SourceSetRow> {
    let sha256: Option<String> = row.get(7)?;
    let last_verified_against = match sha256 {
        Some(sha256) => Some(LastVerified {
            sha256,
            version: row.get(8)?,
            finished_at: row.get(9)?,
        }),
        None => None,
    };
    let mut stale = Vec::new();
    if let Some(walk) = row.get::<_, Option<i64>>(11)? {
        stale.push(SourceSetStale::FilesChanged {
            walk,
            found_at: row.get(12)?,
            changed: row.get::<_, i64>(13)? as u64,
            added: row.get::<_, i64>(14)? as u64,
            removed: row.get::<_, i64>(15)? as u64,
        });
    }
    if let Some(sha256) = row.get::<_, Option<String>>(16)? {
        stale.push(SourceSetStale::NewerRevision {
            sha256,
            version: row.get(17)?,
            imported_at: row.get(18)?,
        });
    }
    let coverage = coverage_from_row(row, 19)?;
    Ok(SourceSetRow {
        id: row.get(0)?,
        dat_id: row.get(1)?,
        record: SourceSetRecord {
            name: row.get(2)?,
            root: row.get(3)?,
            path: row.get(4)?,
            dat: row.get(5)?,
            added_at: row.get(6)?,
            last_verified_against,
            follow: row.get(10)?,
            stale,
            coverage,
        },
    })
}

pub(super) fn set_by_name(c: &Connection, name: &str) -> Result<SourceSetRow> {
    c.query_row(
        &format!("{SET_QUERY} WHERE s.name = ?1 AND s.unregistered_at IS NULL"),
        [name],
        set_from_row,
    )
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no source set named {name}")))
}

pub(super) fn set_by_id(c: &Connection, id: i64) -> Result<SourceSetRow> {
    c.query_row(&format!("{SET_QUERY} WHERE s.id = ?1"), [id], set_from_row)
        .optional()?
        .ok_or_else(|| Error::NotFound(format!("no source set {id}")))
}

/// A Source Set's folder, checked to lie inside its Root.
pub(super) struct Resolved {
    /// The Root's name.
    pub root: String,
    /// Relative to the Root, with `/` separators.
    pub rel: String,
    /// The Root's path, as configured, joined with `rel`.
    pub path: PathBuf,
    /// The Root's canonical path, which every file an Audit reads must resolve inside.
    pub canonical_root: PathBuf,
}

/// Checks that `rel` names a directory inside the Root called `root`.
///
/// Refuses an absolute path, `..`, and a path that, after following symlinks, resolves outside the Root.
pub(super) fn resolve(roots: &crate::roots::Roots, root: &str, rel: &str) -> Result<Resolved> {
    let root_config = roots
        .root(root)
        .ok_or_else(|| Error::Refused(format!("no root named {root} is registered")))?;
    let rel = relative_path(rel)
        .map_err(|e| Error::Refused(format!("{rel}: a source set's path {e}")))?;
    // Defence in depth: the Host's own reading of the path must agree that every component is a plain folder name.
    if !Path::new(&rel)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(Error::Refused(format!(
            "{rel}: a source set's path must be relative to its root"
        )));
    }
    let canonical_root = root_config.path.canonicalize().map_err(|e| {
        Error::Refused(format!("root {root} ({}): {e}", root_config.path.display()))
    })?;
    let configured = rel
        .split('/')
        .fold(root_config.path.clone(), |p, part| p.join(part));
    let canonical = configured
        .canonicalize()
        .map_err(|e| Error::Refused(format!("{}: {e}", configured.display())))?;
    if !canonical.starts_with(&canonical_root) {
        return Err(Error::Refused(format!(
            "{rel}: resolves outside root {root}"
        )));
    }
    if !canonical.is_dir() {
        return Err(Error::Refused(format!(
            "{}: not a directory",
            configured.display()
        )));
    }
    Ok(Resolved {
        root: root.to_owned(),
        rel,
        path: configured,
        canonical_root,
    })
}

impl Store {
    /// The path a Path Template proposes for a Source Set bound to `dat` (`<provider>/<name>`): `template` if given,
    /// else the configured default. The path is not checked against any Root: registering it does that.
    pub fn propose_source_set_path(&self, dat: &str, template: Option<&str>) -> Result<String> {
        let template = match template {
            Some(text) => PathTemplate::parse(text).map_err(|e| Error::Refused(e.to_string()))?,
            None => self.settings()?.path_templates.source_set,
        };
        split_dat(dat)?;
        let dat = self.read(|c| crate::dats::canonical_dat(c, dat))?;
        let (provider, dat_name) = split_dat(&dat)?;
        self.read(|c| {
            crate::dats::catalogued_dat(c, provider, dat_name)?
                .ok_or_else(|| Error::Refused(format!("no DAT {dat} has been imported")))
        })?;
        template
            .render(&TokenValues::for_dat(provider, dat_name))
            .map_err(|e| Error::Refused(e.to_string()))
    }

    /// Registers the folder `rel` in the Root `root` as a Source Set bound to `dat` (`<provider>/<name>`), covering
    /// every Game.
    pub fn add_source_set(&self, root: &str, rel: &str, dat: &str) -> Result<SourceSetRecord> {
        self.add_source_set_covering(root, rel, dat, None)
    }

    /// Registers a Source Set as `add_source_set` does, with the Coverage `coverage`: engine region names the caller
    /// checked against the engine's vocabulary (`title_select_api::coverage::canonical`).
    pub fn add_source_set_covering(
        &self,
        root: &str,
        rel: &str,
        dat: &str,
        coverage: Option<&[String]>,
    ) -> Result<SourceSetRecord> {
        let coverage = coverage_json(coverage)?;
        // Held while it is checked and recorded, so that no Library Folder registered, move approved or Root edited
        // meanwhile comes to overlap it or change the Root it is checked against.
        let _lifecycle = self.lifecycle_lock();
        let roots = &*self.roots();
        if roots.root(root).is_some_and(|r| r.writable) {
            return Err(Error::Refused(format!(
                "root {root} is writable, and only library folders go in a writable root; add the source set from a \
                 read-only root"
            )));
        }
        let resolved = resolve(roots, root, rel)?;
        let canonical = resolved
            .path
            .canonicalize()
            .map_err(|e| Error::Refused(format!("{}: {e}", resolved.path.display())))?;
        let rel = resolved.rel;
        let name = rel.rsplit('/').next().unwrap_or(&rel).to_owned();
        split_dat(dat)?;
        self.write(|tx| {
            if let Some((what, other)) = crate::folders::overlapping(
                tx,
                roots,
                &canonical,
                crate::folders::Registering::SourceSet,
            )? {
                return Err(Error::Refused(format!(
                    "{rel} in root {root} would overlap {what} at {}",
                    other.display()
                )));
            }
            let dat = crate::dats::canonical_dat(tx, dat)?;
            let (provider, dat_name) = split_dat(&dat)?;
            let dat_id: i64 = crate::dats::catalogued_dat(tx, provider, dat_name)?
                .ok_or_else(|| Error::Refused(format!("no DAT {dat} has been imported")))?;
            crate::dats::check_coverable(tx, dat_id, coverage.as_deref())?;
            let clash: Option<String> = tx
                .query_row(
                    "SELECT name FROM source_sets
                     WHERE (name = ?1 OR (root = ?2 AND path = ?3)) AND unregistered_at IS NULL",
                    params![name, root, rel],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(clash) = clash {
                return Err(Error::Refused(format!(
                    "a source set named {clash} already uses that name or folder"
                )));
            }
            tx.execute(
                "INSERT INTO source_sets (name, root, path, dat_id, added_at, coverage)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![name, root, rel, dat_id, now(), coverage],
            )?;
            Ok(set_by_name(tx, &name)?.record)
        })
    }

    /// Every registered Source Set, by name.
    pub fn source_sets(&self) -> Result<Vec<SourceSetRecord>> {
        self.read(|c| {
            let mut stmt = c.prepare(&format!(
                "{SET_QUERY} WHERE s.unregistered_at IS NULL ORDER BY s.name"
            ))?;
            let rows = stmt.query_map([], |r| set_from_row(r).map(|s| s.record))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    pub fn source_set(&self, name: &str) -> Result<SourceSetRecord> {
        self.read(|c| set_by_name(c, name).map(|s| s.record))
    }

    /// Makes the Source Set `name` Follow its DAT, or stop. Following never starts an Audit: a newer DAT Revision
    /// only makes it Stale.
    pub fn set_follow(&self, name: &str, follow: bool) -> Result<SourceSetRecord> {
        self.write(|tx| {
            let watch = crate::observe::StaleWatch::start(tx)?;
            let set = set_by_name(tx, name)?;
            tx.execute(
                "UPDATE source_sets SET follow = ?2 WHERE id = ?1",
                params![set.id, follow],
            )?;
            crate::stale::note_followed_revisions(tx)?;
            watch.finish(tx)?;
            Ok(set_by_name(tx, name)?.record)
        })
    }

    /// Sets the Source Set `name`'s Coverage, checked as for `add_source_set_covering`, or clears it with `None`. It
    /// changes no file and needs no new Audit.
    pub fn set_coverage(&self, name: &str, coverage: Option<&[String]>) -> Result<SourceSetRecord> {
        let coverage = coverage_json(coverage)?;
        self.write(|tx| {
            let set = set_by_name(tx, name)?;
            crate::dats::check_coverable(tx, set.dat_id, coverage.as_deref())?;
            // An approved repair carries out a plan made for the Coverage it had (ADR 0021).
            let repairing: Option<i64> = tx
                .query_row(
                    "SELECT id FROM repair_plans
                     WHERE source_set_id = ?1 AND approved_at IS NOT NULL AND finished_at IS NULL
                     ORDER BY id LIMIT 1",
                    [set.id],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(plan) = repairing {
                return Err(Error::Conflict(format!(
                    "source set {name} has an approved repair, repair plan {plan}, that hasn't finished, and it \
                     repairs the games your current coverage covers; change the coverage once it has finished"
                )));
            }
            tx.execute(
                "UPDATE source_sets SET coverage = ?2 WHERE id = ?1",
                params![set.id, coverage],
            )?;
            Ok(set_by_name(tx, name)?.record)
        })
    }
}

/// The Coverage a `coverage` column holds, in column `i`.
pub(super) fn coverage_from_row(row: &Row, i: usize) -> rusqlite::Result<Option<Vec<String>>> {
    match row.get::<_, Option<String>>(i)? {
        Some(json) => Ok(Some(serde_json::from_str(&json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e))
        })?)),
        None => Ok(None),
    }
}

/// Whether two Coverages cover the same regions, whatever their order.
pub(super) fn same_coverage(a: Option<&[String]>, b: Option<&[String]>) -> bool {
    use std::collections::BTreeSet;
    let regions = |c: &[String]| c.iter().cloned().collect::<BTreeSet<_>>();
    match (a, b) {
        (Some(a), Some(b)) => regions(a) == regions(b),
        (a, b) => a.is_none() && b.is_none(),
    }
}

/// A Coverage as the `coverage` column holds it.
pub(super) fn coverage_json(coverage: Option<&[String]>) -> Result<Option<String>> {
    match coverage {
        Some([]) => Err(Error::Refused(
            "coverage: choose at least one region, or clear the coverage to cover every game"
                .into(),
        )),
        Some(regions) => Ok(Some(
            serde_json::to_string(regions).expect("a list of strings serializes"),
        )),
        None => Ok(None),
    }
}
