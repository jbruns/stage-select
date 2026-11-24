//! Binding a Source Set to another DAT in place. Its registration, name, path, history, kept files and marker stay:
//! every Audit, Selection, Build and kept file names the DAT Revision it used, so only `source_sets.dat_id` changes,
//! with a row of `source_set_dat_changes` to say when and by whom. The Source Set is Stale until it is audited against
//! its new DAT, and each Library Folder bound to it until a Selection of that DAT is built there. A Library Folder's
//! removed games are kept, as they are named by Game and a Game's name usually carries across DATs.

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use stage_select_api::roots::{VoidedPlan, VoidedPlanKind};
use stage_select_api::{DatChange, DatChangeFolder};
use stage_select_engine::dat::Dat;

use crate::sets::{SourceSetRow, set_by_name};
use crate::{Error, Result, Store, now};

/// What binding a Source Set to another DAT would do, as the store is now.
struct Assessed {
    set: SourceSetRow,
    to_id: i64,
    to: String,
    /// The SHA-256 of the newest revision of the DAT it would be bound to, whose Games say whether it keeps its
    /// Coverage.
    newest: String,
    voidable: Vec<VoidedPlan>,
    folders: Vec<DatChangeFolder>,
}

/// Whether a Coverage of `coverage` still covers any of `games`, each read for its regions by `regions`, by the
/// Coverage's own rule (World always covered, United Kingdom as UK). A DAT none of whose Games is covered would
/// cover nothing.
pub(crate) fn keeps_coverage(
    games: &[String],
    coverage: &[String],
    regions: &dyn Fn(&str) -> Vec<String>,
) -> bool {
    games
        .iter()
        .any(|game| stage_select_api::coverage::covered(&regions(game), Some(coverage)))
}

/// Why voided work's Job ended, when Source Set `set` was bound to another DAT after it was approved.
fn void_reason(set: &str) -> String {
    format!(
        "voided: you bound source set {set} to another DAT after this plan was approved, and the plan was made \
         from its old DAT; nothing was changed"
    )
}

fn refuse<T>(set: &str, why: String) -> Result<T> {
    Err(Error::Conflict(format!(
        "Source set {set} can't be bound to another DAT: {why}."
    )))
}

fn assess(c: &Connection, name: &str, dat: &str) -> Result<Assessed> {
    let set = set_by_name(c, name)?;
    let dat = crate::dats::canonical_dat(c, dat)?;
    let (provider, dat_name) = crate::dats::split_dat(&dat)?;
    let to_id = crate::dats::catalogued_dat(c, provider, dat_name)?.ok_or_else(|| {
        Error::NotFound(format!(
            "There is no DAT {dat}: import a revision of it first."
        ))
    })?;
    if to_id == set.dat_id {
        return refuse(name, format!("it is already bound to {dat}"));
    }
    let (from, to) = (
        crate::dats::dat_format(c, set.dat_id)?,
        crate::dats::dat_format(c, to_id)?,
    );
    if from != to {
        return refuse(
            name,
            format!(
                "{dat} is a {} DAT and {} is a {}; choose a DAT of the same kind, or register the folder as \
                 another source set",
                to.map_or("different", |f| f.as_str()),
                set.record.dat,
                from.map_or("different", |f| f.as_str())
            ),
        );
    }
    let placed: Option<(i64, String, String)> = c
        .query_row(
            "SELECT count(*), min(f.root), min(f.path) FROM placed_games p
             JOIN library_folders f ON f.id = p.folder_id WHERE p.source_set_id = ?1
             HAVING count(*) > 0",
            [set.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    if let Some((games, root, path)) = placed {
        return refuse(
            name,
            format!(
                "{games} of its games are placed in play library folder {root}/{path} under single copy. Send \
                 them back first: choose them under the folder's built files and choose Send back to source, or \
                 turn single copy off for the system and build the folder again"
            ),
        );
    }
    let folders: Vec<(i64, String, String, bool)> = c
        .prepare(
            "SELECT id, root, path, current_build_id IS NOT NULL FROM library_folders
             WHERE source_set_id = ?1 AND unregistered_at IS NULL ORDER BY root, path",
        )?
        .query_map([set.id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut voidable = Vec::new();
    let builds: Vec<(i64, i64, String, String)> = c
        .prepare(
            "SELECT j.id, j.plan_id, f.root, f.path FROM jobs j JOIN library_folders f ON f.id = j.library_folder_id
             WHERE j.kind = 'build' AND j.state = 'queued' AND f.source_set_id = ?1 ORDER BY j.id",
        )?
        .query_map([set.id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (job, plan, root, path) in builds {
        voidable.push(VoidedPlan {
            kind: VoidedPlanKind::Build,
            plan,
            job,
            writes_to: format!("play library folder {root}/{path}"),
        });
    }
    // An approved repair whose Job hasn't started, and that has changed nothing yet, is voidable; any other is busy.
    let repairs: Vec<(i64, Option<i64>, bool)> = c
        .prepare(
            "SELECT p.id,
                    (SELECT j.id FROM jobs j WHERE j.repair_plan_id = p.id AND j.state = 'queued'
                     ORDER BY j.id DESC LIMIT 1),
                    NOT EXISTS (SELECT 1 FROM jobs j WHERE j.repair_plan_id = p.id AND j.state = 'running')
                    AND NOT EXISTS (SELECT 1 FROM repair_steps st WHERE st.plan_id = p.id
                                    AND NOT (st.state = 'pending'
                                             OR (st.state = 'moving' AND EXISTS (
                                                 SELECT 1 FROM ingest_moves m WHERE m.id = st.move_id
                                                 AND m.state = 'pending'))))
             FROM repair_plans p
             WHERE p.source_set_id = ?1 AND p.approved_at IS NOT NULL AND p.finished_at IS NULL
             ORDER BY p.id",
        )?
        .query_map([set.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut busy = Vec::new();
    for (plan, queued, untouched) in repairs {
        match (queued, untouched) {
            (Some(job), true) => voidable.push(VoidedPlan {
                kind: VoidedPlanKind::Repair,
                plan,
                job,
                writes_to: format!("source set {name}"),
            }),
            _ => busy.push(format!("repair plan {plan} is approved and has started")),
        }
    }
    let jobs: Vec<(i64, String, String)> = c
        .prepare(
            "SELECT j.id, replace(j.kind, '_', ' '), j.state FROM jobs j
             WHERE j.state IN ('queued', 'running')
               AND (j.source_set_id = ?1
                    OR j.library_folder_id IN (SELECT id FROM library_folders WHERE source_set_id = ?1))
             ORDER BY j.id",
        )?
        .query_map([set.id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (job, kind, state) in jobs {
        if !voidable.iter().any(|v| v.job == job) {
            busy.push(format!("{kind} job {job} is {state}"));
        }
    }
    busy.extend(crate::lifecycle::strings(
        c,
        "SELECT DISTINCT 'ingest plan ' || m.plan_id || ' is still moving files into it'
         FROM ingest_moves m JOIN source_sets s ON s.root = m.dest_root AND s.path = m.dest_rel
         WHERE s.id = ?1 AND m.plan_id IS NOT NULL AND m.dest_kind = 'path' AND m.state NOT IN ('done', 'failed')",
        [set.id],
    )?);
    if !busy.is_empty() {
        return refuse(
            name,
            format!("{}. Try again once that has finished", busy.join("; ")),
        );
    }
    let newest: String = c.query_row(
        "SELECT sha256 FROM dat_revisions WHERE dat_id = ?1 AND deleted_at IS NULL
         ORDER BY header_order DESC, import_seq DESC LIMIT 1",
        [to_id],
        |r| r.get(0),
    )?;
    Ok(Assessed {
        set,
        to_id,
        to: dat,
        newest,
        voidable,
        folders: folders
            .into_iter()
            .filter(|(.., built)| *built)
            .map(|(id, root, path, _)| DatChangeFolder { id, root, path })
            .collect(),
    })
}

impl Store {
    /// The Coverage `assessed` clears: its Source Set's, when no Game of the newest revision of the new DAT is in
    /// any of its regions.
    fn cleared_coverage(&self, assessed: &Assessed) -> Result<Option<Vec<String>>> {
        let Some(coverage) = &assessed.set.record.coverage else {
            return Ok(None);
        };
        let regions = self.region_reader().map_err(|_| {
            Error::RegionsUnavailable(format!(
                "source set {} has a coverage, and whether {} has games in its regions needs the selection \
                 engine's clone lists, which aren't available; install them, or clear the coverage",
                assessed.set.record.name, assessed.to
            ))
        })?;
        let dat =
            Dat::read(&self.blob_path(&assessed.newest)).map_err(|e| Error::Io(e.to_string()))?;
        Ok((!keeps_coverage(&dat.games, coverage, &*regions)).then(|| coverage.clone()))
    }

    fn dat_change_of(&self, assessed: Assessed, cleared: Option<Vec<String>>) -> DatChange {
        DatChange {
            from: assessed.set.record.dat.clone(),
            source_set: assessed.set.record,
            to: assessed.to,
            cleared_coverage: cleared,
            voided: assessed.voidable,
            library_folders: assessed.folders,
        }
    }

    /// What binding the Source Set `name` to the DAT `dat`, `<provider>/<name>`, would do, changing nothing; refused
    /// as [`Store::change_source_set_dat`] would refuse it.
    pub fn dat_change(&self, name: &str, dat: &str) -> Result<DatChange> {
        let assessed = self.read(|c| assess(c, name, dat))?;
        let cleared = self.cleared_coverage(&assessed)?;
        Ok(self.dat_change_of(assessed, cleared))
    }

    /// Binds the Source Set `name` to the DAT `dat`, `<provider>/<name>`, in place, as `actor`. Refused with
    /// [`Error::Conflict`] while work it can't void is under way on the Source Set or a Library Folder bound to it,
    /// while any of its Games is placed under Single Copy, when `dat` is of another DAT Format, or is the DAT it has.
    /// Voids each approved plan, not yet started, that the Source Set's old DAT made; clears its Coverage when no
    /// Game of `dat` is in its regions; and keeps Follow as it is.
    pub fn change_source_set_dat(&self, name: &str, dat: &str, actor: &str) -> Result<DatChange> {
        let _lifecycle = self.lifecycle_lock();
        let before = self.read(|c| assess(c, name, dat))?;
        let cleared = self.cleared_coverage(&before)?;
        let (assessed, record) = self.write(|tx| {
            let watch = crate::observe::StaleWatch::start(tx)?;
            let assessed = assess(tx, name, dat)?;
            if assessed.newest != before.newest || assessed.set.record.coverage != before.set.record.coverage {
                return Err(Error::Conflict(format!(
                    "Source set {name} or the DAT {} changed while it was being bound to it; nothing was changed. \
                     Try again.",
                    assessed.to
                )));
            }
            let why = void_reason(name);
            for plan in &assessed.voidable {
                crate::roots::void(tx, plan, &why, &format!("source set {name}"))?;
            }
            apply(tx, &assessed, cleared.is_some(), actor)?;
            watch.finish(tx)?;
            Ok((assessed, set_by_name(tx, name)?.record))
        })?;
        let mut change = self.dat_change_of(assessed, cleared);
        change.source_set = record;
        Ok(change)
    }
}

fn apply(tx: &Transaction, assessed: &Assessed, clear: bool, actor: &str) -> Result<()> {
    let set = assessed.set.id;
    tx.execute(
        "UPDATE source_sets SET dat_id = ?2, coverage = CASE WHEN ?3 THEN NULL ELSE coverage END WHERE id = ?1",
        params![set, assessed.to_id, clear],
    )?;
    tx.execute(
        "INSERT INTO source_set_dat_changes (source_set_id, from_dat_id, to_dat_id, actor, changed_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![set, assessed.set.dat_id, assessed.to_id, actor, now()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::keeps_coverage;

    fn regions(game: &str) -> Vec<String> {
        match game {
            "Asteroids (USA)" => vec!["USA".into()],
            "Asteroids (Europe)" => vec!["Europe".into()],
            "Asteroids (USA, Europe)" => vec!["USA".into(), "Europe".into()],
            "Asteroids (World)" => vec!["World".into()],
            "Asteroids (Japan)" => vec!["Japan".into()],
            "Asteroids (United Kingdom)" => vec!["United Kingdom".into()],
            _ => Vec::new(),
        }
    }

    fn games(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn a_coverage_stays_while_a_game_is_in_one_of_its_regions() {
        let usa = ["USA".to_owned()];
        assert!(keeps_coverage(
            &games(&["Asteroids (Europe)", "Asteroids (USA, Europe)"]),
            &usa,
            &regions
        ));
        assert!(!keeps_coverage(
            &games(&["Asteroids (Europe)", "Unknown"]),
            &usa,
            &regions
        ));
        assert!(!keeps_coverage(&[], &usa, &regions));
    }

    #[test]
    fn a_coverage_stays_by_coverages_own_rules_for_world_and_the_united_kingdom() {
        let usa = ["USA".to_owned()];
        assert!(keeps_coverage(
            &games(&["Asteroids (World)", "Asteroids (Japan)"]),
            &usa,
            &regions
        ));
        let uk = ["UK".to_owned()];
        assert!(keeps_coverage(
            &games(&["Asteroids (United Kingdom)", "Asteroids (Japan)"]),
            &uk,
            &regions
        ));
        assert!(!keeps_coverage(
            &games(&["Asteroids (Japan)"]),
            &uk,
            &regions
        ));
    }
}
