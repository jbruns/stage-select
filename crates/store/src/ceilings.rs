//! The Ceilings of the Host and each Root (ADR 0022): where each comes from, what the pace allows of it now, and the
//! pools every Job shares to keep to it.
//!
//! Each Ceiling takes the first of these that has a value: the Operator's value, saved in the `ceilings` settings
//! section for the Host and with each Root for its own; the latest Calibration's knee; the CPU count. A Root's readers
//! fall back to the Host's readers value, if the Operator set one, before the Calibration. Byte rates have no
//! CPU-count fallback, and 7z archive workers, which aren't measured, fall back to a fixed default.
//!
//! The resolved Ceilings are worked out again when the settings, a Root or a Calibration change them, never while a
//! Job reads: each worker takes a permit from a pool whose size is already the pace's share, at its next file.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use rusqlite::Connection;
use stage_select_api::calibration::Calibration;
use stage_select_api::settings::{
    Ceiling, CeilingInput, CeilingOverrides, CeilingSource, CeilingsUpdate,
    DEFAULT_ARCHIVE_WORKERS, HostCeilings, HostOverrides, JobsView, MAX_ARCHIVE_WORKERS,
    MAX_MIB_PER_S, MAX_WORKERS, Pace, RootCeilings, RootOverrides,
};
use stage_select_engine::Pools;
use stage_select_engine::cap::Caps;
use stage_select_engine::pool::Pool;
use stage_select_engine::sevenz::Gate;

use crate::roots::Roots;
use crate::{Error, Result, Store};

const MIB: f64 = 1024.0 * 1024.0;

/// What the Ceilings are worked out from, besides each Root's own.
pub struct Sources<'a> {
    /// The Operator's values for the Host, saved in the workbench.
    pub yours: &'a HostOverrides,
    pub calibration: &'a Calibration,
    /// The CPUs the Service can use.
    pub cpus: usize,
    pub pace: Pace,
    /// The fastest write throughput past Jobs averaged into each Root, in MiB/s.
    pub observed_writes: &'a BTreeMap<String, u64>,
}

/// One Ceiling from its sources, first first: the Operator's, the measured one, then the fallback. `in_force` is the
/// pace's share of it.
fn ceiling(
    yours: Option<u64>,
    measured: Option<u64>,
    fallback: Option<(u64, CeilingSource)>,
    in_force: impl Fn(u64) -> Option<u64>,
) -> Ceiling {
    let (value, source) = match (yours, measured, fallback) {
        (Some(value), ..) => (Some(value), Some(CeilingSource::Yours)),
        (None, Some(value), _) => (Some(value), Some(CeilingSource::Calibration)),
        (None, None, Some((value, source))) => (Some(value), Some(source)),
        (None, None, None) => (None, None),
    };
    Ceiling {
        ceiling: value,
        in_force: value.and_then(in_force),
        source,
        yours,
        measured,
    }
}

/// Every Ceiling of the Host and of each Root in `roots`, and what `sources.pace` allows of each.
pub fn resolve(roots: &Roots, sources: &Sources) -> JobsView {
    let pace = sources.pace;
    let workers = |c: u64| Some(pace.of(c as usize) as u64);
    let rate = |c: u64| pace.rate(Some(c));
    let cpus = Some((sources.cpus.max(1) as u64, CeilingSource::Cpus));
    let yours = sources.yours;
    let host_knee = sources
        .calibration
        .host
        .as_ref()
        .and_then(|h| h.knee)
        .map(|k| k.workers as u64);
    let host = HostCeilings {
        concurrent: ceiling(yours.concurrent, None, cpus, workers),
        hashers: ceiling(yours.hashers, host_knee, cpus, workers),
        readers: ceiling(yours.readers, None, None, workers),
        archive_workers: ceiling(
            yours.archive_workers,
            None,
            Some((DEFAULT_ARCHIVE_WORKERS, CeilingSource::Default)),
            workers,
        ),
    };
    let roots = roots
        .iter()
        .map(|root| {
            let name = &root.name;
            let knee = sources
                .calibration
                .roots
                .iter()
                .find(|r| &r.root == name)
                .and_then(|r| r.knee);
            RootCeilings {
                root: name.clone(),
                readers: ceiling(
                    root.readers.or(yours.readers),
                    knee.map(|k| k.workers as u64),
                    cpus,
                    workers,
                ),
                read_mib_per_s: ceiling(
                    root.read_mib_per_s,
                    knee.map(|k| ((k.bytes_per_s / MIB) as u64).max(1)),
                    None,
                    rate,
                ),
                write_mib_per_s: ceiling(root.write_mib_per_s, None, None, rate),
                observed_write_mib_per_s: sources.observed_writes.get(name).copied(),
            }
        })
        .collect();
    JobsView { pace, host, roots }
}

/// The fastest write throughput a finished Job averaged into each Root, in MiB/s and at least 1: Builds into a
/// Library Folder, repairs of a Source Set and Additions into an Additions Set.
pub(crate) fn observed_writes(c: &Connection) -> Result<BTreeMap<String, u64>> {
    let rows = c
        .prepare(
            "SELECT root, max(bytes_written * 1000.0 / duration_ms) FROM (
                 SELECT CASE j.kind WHEN 'build' THEN lf.root WHEN 'repair' THEN s.root
                                    WHEN 'additions' THEN a.root END AS root,
                        j.bytes_written, j.duration_ms
                 FROM jobs j
                 LEFT JOIN library_folders lf ON lf.id = j.library_folder_id
                 LEFT JOIN source_sets s ON s.id = j.source_set_id
                 LEFT JOIN additions_sets a ON a.id = j.additions_set_id
                 WHERE j.kind IN ('build', 'repair', 'additions') AND j.bytes_written > 0 AND j.duration_ms > 0)
             WHERE root IS NOT NULL GROUP BY root",
        )?
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(root, bytes_per_s)| (root, ((bytes_per_s / MIB) as u64).max(1)))
        .collect())
}

/// Checks `update` against `roots` and returns the Operator's Ceilings it sets: each a whole number in range, for a
/// Root `roots` holds.
pub(crate) fn checked(roots: &Roots, update: &CeilingsUpdate) -> Result<CeilingOverrides> {
    let host = &update.host;
    let host = HostOverrides {
        concurrent: value(&host.concurrent, "the most jobs at once", MAX_WORKERS)?,
        hashers: value(&host.hashers, "the hash workers", MAX_WORKERS)?,
        readers: value(&host.readers, "the readers on each root", MAX_WORKERS)?,
        archive_workers: value(
            &host.archive_workers,
            "the 7z archive workers",
            MAX_ARCHIVE_WORKERS,
        )?,
    };
    let mut overrides = BTreeMap::new();
    for (name, update) in &update.roots {
        if roots.root(name).is_none() {
            return Err(Error::Refused(format!("There is no root {name}.")));
        }
        let root = RootOverrides {
            readers: value(
                &update.readers,
                &format!("the readers on {name}"),
                MAX_WORKERS,
            )?,
            read_mib_per_s: value(
                &update.read_mib_per_s,
                &format!("the read rate of {name}"),
                MAX_MIB_PER_S,
            )?,
            write_mib_per_s: value(
                &update.write_mib_per_s,
                &format!("the write rate of {name}"),
                MAX_MIB_PER_S,
            )?,
        };
        overrides.insert(name.clone(), root);
    }
    Ok(CeilingOverrides {
        host,
        roots: overrides,
    })
}

/// One Ceiling as given: none when null or absent, else a whole number from 1 to `max`.
fn value(given: &CeilingInput, what: &str, max: u64) -> Result<Option<u64>> {
    let given = match given {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(given) => given,
    };
    match given.as_u64() {
        Some(v) if (1..=max).contains(&v) => Ok(Some(v)),
        _ => Err(Error::Refused(format!(
            "Set {what} to a whole number from 1 to {max}, or leave it empty to use the default."
        ))),
    }
}

/// The pools and limits every Job shares, sized to what the pace allows of each Ceiling now.
pub(crate) struct Live {
    hashers: Arc<Pool>,
    archives: Gate,
    /// One pool for each Root, made as a Job first reads it.
    pub(crate) readers: Arc<RootPools>,
    concurrent: AtomicUsize,
    view: RwLock<Option<JobsView>>,
    ready: AtomicBool,
    /// Held while the Ceilings are worked out and applied, so that two refreshes apply in the order they read.
    refreshing: Mutex<()>,
}

impl Default for Live {
    fn default() -> Self {
        Live {
            hashers: Arc::new(Pool::new(1)),
            archives: Gate::new(1),
            readers: Arc::default(),
            concurrent: AtomicUsize::new(1),
            view: RwLock::default(),
            ready: AtomicBool::new(false),
            refreshing: Mutex::default(),
        }
    }
}

impl Live {
    fn apply(&self, view: JobsView) {
        let in_force = |c: &Ceiling| c.in_force.unwrap_or(1) as usize;
        self.hashers.set_limit(in_force(&view.host.hashers));
        self.archives
            .set_permits(in_force(&view.host.archive_workers));
        self.concurrent
            .store(in_force(&view.host.concurrent), Ordering::SeqCst);
        {
            let unnamed = view.pace.of(crate::calibration::cpus());
            self.readers.unnamed.store(unnamed, Ordering::SeqCst);
            let mut readers = self.readers.map.write().unwrap_or_else(|e| e.into_inner());
            for (name, pool) in readers.iter() {
                if !view.roots.iter().any(|r| &r.root == name) {
                    pool.set_limit(unnamed);
                }
            }
            for root in &view.roots {
                let limit = in_force(&root.readers);
                match readers.get(&root.root) {
                    Some(pool) => pool.set_limit(limit),
                    None => {
                        readers.insert(root.root.clone(), Arc::new(Pool::new(limit)));
                    }
                }
            }
        }
        {
            // Each bucket takes its new rate at once: a wait under way rescales what it has left at its next look.
            let bytes = |c: &Ceiling| c.in_force.map(|mib| mib.saturating_mul(1 << 20));
            let mut caps = self.readers.caps.write().unwrap_or_else(|e| e.into_inner());
            for (name, cap) in caps.iter() {
                if !view.roots.iter().any(|r| &r.root == name) {
                    cap.set(None, None);
                }
            }
            for root in &view.roots {
                let (read, write) = (bytes(&root.read_mib_per_s), bytes(&root.write_mib_per_s));
                caps.entry(root.root.clone()).or_default().set(read, write);
            }
        }
        *self.view.write().unwrap_or_else(|e| e.into_inner()) = Some(view);
        self.ready.store(true, Ordering::Release);
    }
}

/// Each Root's shared readers pool, which every Job reading the Root takes its permits from.
#[derive(Debug)]
pub(crate) struct RootPools {
    map: RwLock<BTreeMap<String, Arc<Pool>>>,
    /// The limit of a Root not registered: the pace's share of the CPU count.
    unnamed: AtomicUsize,
    /// Each Root's byte-rate caps, at the pace's share of its byte-rate Ceilings.
    caps: RwLock<BTreeMap<String, Arc<Caps>>>,
}

impl Default for RootPools {
    fn default() -> Self {
        RootPools {
            map: RwLock::default(),
            caps: RwLock::default(),
            unnamed: AtomicUsize::new(crate::calibration::cpus()),
        }
    }
}

impl stage_select_engine::meter::RootLimits for RootPools {
    fn caps(&self, root: &str) -> Option<Arc<Caps>> {
        if let Some(caps) = self
            .caps
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(root)
        {
            return Some(caps.clone());
        }
        // A Root not registered has no byte-rate Ceiling, so no cap.
        Some(
            self.caps
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .entry(root.to_owned())
                .or_default()
                .clone(),
        )
    }

    fn readers(&self, root: &str) -> Arc<Pool> {
        if let Some(pool) = self.map.read().unwrap_or_else(|e| e.into_inner()).get(root) {
            return pool.clone();
        }
        self.map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(root.to_owned())
            .or_insert_with(|| Arc::new(Pool::new(self.unnamed.load(Ordering::SeqCst))))
            .clone()
    }
}

/// A Job's pools: its Root's readers and the Host's hash workers.
pub struct JobPools {
    readers: Arc<Pool>,
    hashers: Arc<Pool>,
}

impl JobPools {
    /// Lets `meter` report this Job's allowance from these pools.
    pub fn meter(&self, meter: &stage_select_engine::meter::JobMeter) {
        meter.shares(self.readers.clone(), self.hashers.clone());
    }

    pub fn pools(&self) -> Pools<'_> {
        Pools {
            readers: &self.readers,
            hashers: &self.hashers,
        }
    }
}

impl Store {
    /// Works the Ceilings out again, from the Roots, the settings, the latest Calibration and past Jobs' writes, and
    /// applies them to every Job from its workers' next file. Run when any of those change.
    pub fn refresh_ceilings(&self) -> Result<JobsView> {
        let _one_at_a_time = self
            .ceilings
            .refreshing
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let cpus = crate::calibration::cpus();
        let calibration = self.calibration(cpus)?;
        let (yours, observed_writes) =
            self.read(|c| Ok((crate::settings::load(c)?.ceilings, observed_writes(c)?)))?;
        let view = resolve(
            &self.roots(),
            &Sources {
                yours: &yours,
                calibration: &calibration,
                cpus,
                pace: self.pace(),
                observed_writes: &observed_writes,
            },
        );
        self.ceilings.apply(view.clone());
        Ok(view)
    }

    /// The Ceilings in force: worked out when first asked for, and again whenever they change.
    pub fn ceilings(&self) -> Result<JobsView> {
        self.live_ceilings()?;
        Ok(self
            .ceilings
            .view
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("ceilings are worked out once ready"))
    }

    fn live_ceilings(&self) -> Result<&Live> {
        if !self.ceilings.ready.load(Ordering::Acquire) {
            self.refresh_ceilings()?;
        }
        Ok(&self.ceilings)
    }

    /// The Ceilings a Job starts with: the largest reader Ceiling of any Root, and the Host's hash workers. A Job that
    /// reads through the shared pools starts its threads as they let them work, whatever these say, so a Ceiling or
    /// pace changed while it runs reaches it at each worker's next file; these are what it records, and what the
    /// pace's share is taken of where it reads without them.
    pub fn job_workers(&self) -> stage_select_api::Workers {
        let cpus = crate::calibration::cpus();
        let Ok(view) = self.ceilings() else {
            return stage_select_api::Workers {
                readers: cpus,
                hashers: cpus,
            };
        };
        let ceiling = |c: &Ceiling| c.ceiling.unwrap_or(1) as usize;
        stage_select_api::Workers {
            readers: view
                .roots
                .iter()
                .map(|r| ceiling(&r.readers))
                .max()
                .unwrap_or(cpus),
            hashers: ceiling(&view.host.hashers),
        }
    }

    /// How many Jobs may run at once now: the pace's share of the Host's Ceiling.
    pub fn concurrent_in_force(&self) -> Result<usize> {
        Ok(self.live_ceilings()?.concurrent.load(Ordering::SeqCst))
    }

    /// The pools a Job reading the Root `root` shares with every other Job.
    pub fn job_pools(&self, root: &str) -> Result<JobPools> {
        let live = self.live_ceilings()?;
        // A Root not registered is read with the readers of the CPU count, at this pace.
        Ok(JobPools {
            readers: stage_select_engine::meter::RootLimits::readers(&*live.readers, root),
            hashers: live.hashers.clone(),
        })
    }

    /// The threads chdman compresses with: the pace's share of the hash workers Ceiling, or chdman's own choice of
    /// every core when `unthrottled`.
    pub(crate) fn chdman_threads(&self) -> Result<Option<usize>> {
        self.live_ceilings()?;
        Ok(self.chdman_threads_now())
    }

    /// [`Store::chdman_threads`] as the Ceilings stand now, without looking at the database: for each chdman as it
    /// starts, so that a pace changed during a Build reaches its next CHD. One already running keeps its threads.
    pub(crate) fn chdman_threads_now(&self) -> Option<usize> {
        let view = self.ceilings.view.read().unwrap_or_else(|e| e.into_inner());
        match view.as_ref() {
            Some(view) => view
                .pace
                .chdman_threads(view.host.hashers.ceiling.unwrap_or(1) as usize),
            None => Some(1),
        }
    }

    /// Each Root's byte-rate caps in force now, in bytes a second, by name: none for a Root under `unthrottled`, or
    /// without a byte-rate Ceiling.
    pub fn byte_caps(&self) -> BTreeMap<String, stage_select_api::ByteCaps> {
        self.ceilings
            .readers
            .caps
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(root, caps)| {
                let caps = stage_select_api::ByteCaps {
                    read_bytes_per_s: caps.read.rate(),
                    write_bytes_per_s: caps.write.rate(),
                };
                (root.clone(), caps)
            })
            .collect()
    }

    /// The gate every 7z archive decodes through, its size the pace's share of the archive workers Ceiling.
    pub(crate) fn archive_gate(&self) -> &Gate {
        match self.live_ceilings() {
            Ok(live) => &live.archives,
            // The Ceilings can't be worked out: the gate keeps the size it had, one archive at a time at first.
            Err(_) => &self.ceilings.archives,
        }
    }
}

#[cfg(test)]
mod tests {
    use stage_select_api::ByteCaps;
    use stage_select_api::calibration::{HostCalibration, Knee, RootCalibration, RootOutcome};
    use stage_select_api::settings::{HostCeilingsUpdate, RootCeilingsUpdate, SettingsUpdate};
    use stage_select_engine::meter::RootLimits;

    use super::*;

    /// A Root named `name` with the Operator's Ceilings `[readers, read, write]`.
    fn root(name: &str, [readers, read, write]: [Option<u64>; 3]) -> crate::roots::Root {
        crate::roots::Root {
            name: name.into(),
            path: format!("/{name}").into(),
            kind: crate::roots::RootKind::Sources,
            writable: false,
            writable_paths: Vec::new(),
            readers,
            read_mib_per_s: read,
            write_mib_per_s: write,
            updated_by: "test".into(),
            updated_at: "then".into(),
        }
    }

    fn roots(roots: impl IntoIterator<Item = crate::roots::Root>) -> Roots {
        Roots::of(roots)
    }

    fn calibrated(hashers: usize, readers: usize, mib_per_s: f64) -> Calibration {
        let knee = |workers, mib: f64| {
            Some(Knee {
                workers,
                bytes_per_s: mib * MIB,
            })
        };
        Calibration {
            host: Some(HostCalibration {
                job: 1,
                measured_at: "2026-01-01T00:00:00Z".into(),
                cpus: 8,
                curve: Vec::new(),
                knee: knee(hashers, 900.0),
            }),
            roots: vec![RootCalibration {
                root: "sets".into(),
                path: "/sets".into(),
                job: 1,
                measured_at: "2026-01-01T00:00:00Z".into(),
                outcome: RootOutcome::Measured,
                available_bytes: 1 << 30,
                curve: Vec::new(),
                knee: knee(readers, mib_per_s),
                error: None,
            }],
            stale: Vec::new(),
        }
    }

    fn resolved(
        roots: &Roots,
        yours: &HostOverrides,
        calibration: &Calibration,
        pace: Pace,
    ) -> JobsView {
        resolve(
            roots,
            &Sources {
                yours,
                calibration,
                cpus: 8,
                pace,
                observed_writes: &BTreeMap::from([("sets".to_owned(), 70)]),
            },
        )
    }

    fn of(c: &Ceiling) -> (Option<u64>, Option<CeilingSource>) {
        (c.ceiling, c.source)
    }

    #[test]
    fn the_write_rate_offered_is_the_fastest_a_past_job_averaged_into_each_root() {
        let (_dir, store) = store();
        let observed = store
            .write(|tx| {
                tx.execute_batch(
                    "INSERT INTO dats (provider, name) VALUES ('Test', 'Test');
                     INSERT INTO dat_revisions (dat_id, sha256, version, file_name, imported_at)
                         SELECT id, 'abc', '1', 'Test.dat', 'then' FROM dats;
                     INSERT INTO source_sets (name, root, path, dat_id, added_at)
                         SELECT 'Set', 'sets', 'Set', id, 'then' FROM dats;",
                )?;
                let job = |kind: &str, plan: Option<i64>, written: i64, ms: i64| {
                    tx.execute(
                        "INSERT INTO jobs (state, kind, source_set_id, dat_revision_id, repair_plan_id, actor,
                                           readers, hashers, created_at, bytes_written, duration_ms)
                         SELECT 'succeeded', ?1, s.id, r.id, ?2, 'Operator', 1, 1, 'then', ?3, ?4
                         FROM source_sets s, dat_revisions r",
                        rusqlite::params![kind, plan, written, ms],
                    )
                    .map(|_| tx.last_insert_rowid())
                };
                // The audit the repair plan was made from, which wrote nothing.
                let audit_job = job("audit", None, 0, 1_000)?;
                tx.execute(
                    "INSERT INTO audits (source_set_id, dat_revision_id, status, source_set_path, started_at, job_id,
                                         evidence)
                     SELECT s.id, r.id, 'finished', 'Set', 'then', ?1, 'full' FROM source_sets s, dat_revisions r",
                    [audit_job],
                )?;
                tx.execute(
                    "INSERT INTO repair_plans (source_set_id, audit_id, content, digest, created_at, readers, hashers)
                     SELECT source_set_id, id, '{}', 'digest', 'then', 0, 0 FROM audits",
                    [],
                )?;
                let plan = tx.last_insert_rowid();
                for (written, ms) in [(100i64 << 20, 2_000i64), (300 << 20, 2_000), (0, 1_000)] {
                    job("repair", Some(plan), written, ms)?;
                }
                // An audit's write, were one counted, would be the fastest.
                job("audit", None, 900 << 20, 1_000)?;
                observed_writes(tx)
            })
            .unwrap();
        assert_eq!(
            observed,
            BTreeMap::from([("sets".to_owned(), 150)]),
            "an audit writes nothing to count, and a job that wrote nothing is left out"
        );
    }

    #[test]
    fn each_ceiling_takes_yours_then_the_calibration_then_the_cpu_count() {
        use CeilingSource as S;
        let bare = roots([root("sets", [None; 3])]);
        let none = HostOverrides::default();
        let unmeasured = Calibration::default();
        let view = resolved(&bare, &none, &unmeasured, Pace::Unthrottled);
        assert_eq!(of(&view.host.concurrent), (Some(8), Some(S::Cpus)));
        assert_eq!(of(&view.host.hashers), (Some(8), Some(S::Cpus)));
        assert_eq!(of(&view.host.readers), (None, None), "only yours");
        assert_eq!(of(&view.host.archive_workers), (Some(2), Some(S::Default)));
        let sets = &view.roots[0];
        assert_eq!(of(&sets.readers), (Some(8), Some(S::Cpus)));
        // A byte rate has no CPU-count fallback: an uncalibrated Root has no read ceiling.
        assert_eq!(of(&sets.read_mib_per_s), (None, None));
        assert_eq!(of(&sets.write_mib_per_s), (None, None));
        assert_eq!(sets.observed_write_mib_per_s, Some(70));

        let measured = calibrated(3, 2, 250.5);
        let view = resolved(&bare, &none, &measured, Pace::Unthrottled);
        assert_eq!(of(&view.host.hashers), (Some(3), Some(S::Calibration)));
        assert_eq!(
            of(&view.host.concurrent),
            (Some(8), Some(S::Cpus)),
            "not measured"
        );
        let sets = &view.roots[0];
        assert_eq!(of(&sets.readers), (Some(2), Some(S::Calibration)));
        assert_eq!(of(&sets.read_mib_per_s), (Some(250), Some(S::Calibration)));

        let yours = HostOverrides {
            concurrent: Some(3),
            hashers: Some(5),
            readers: None,
            archive_workers: Some(4),
        };
        let set = roots([root("sets", [Some(6), Some(100), Some(40)])]);
        let view = resolved(&set, &yours, &measured, Pace::Unthrottled);
        assert_eq!(of(&view.host.hashers), (Some(5), Some(S::Yours)));
        assert_eq!(view.host.hashers.measured, Some(3));
        assert_eq!(of(&view.host.archive_workers), (Some(4), Some(S::Yours)));
        let sets = &view.roots[0];
        assert_eq!(of(&sets.readers), (Some(6), Some(S::Yours)));
        assert_eq!(sets.readers.measured, Some(2));
        assert_eq!(of(&sets.read_mib_per_s), (Some(100), Some(S::Yours)));
        assert_eq!(of(&sets.write_mib_per_s), (Some(40), Some(S::Yours)));
    }

    #[test]
    fn your_readers_for_the_host_is_the_reader_ceiling_of_every_root_without_its_own() {
        let yours = HostOverrides {
            readers: Some(3),
            ..HostOverrides::default()
        };
        let view = resolved(
            &roots([root("a", [None; 3]), root("b", [Some(5), None, None])]),
            &yours,
            &calibrated(4, 4, 100.0),
            Pace::Unthrottled,
        );
        let readers: Vec<_> = view.roots.iter().map(|r| r.readers.ceiling).collect();
        assert_eq!(readers, [Some(3), Some(5)]);
        assert_eq!(view.host.readers.ceiling, Some(3));
    }

    #[test]
    fn the_pace_scales_every_ceiling_and_unthrottled_sets_no_byte_cap() {
        let yours = HostOverrides {
            concurrent: Some(8),
            hashers: Some(8),
            readers: None,
            archive_workers: Some(8),
        };
        let set = roots([root("sets", [Some(8), Some(400), Some(3)])]);
        let unmeasured = Calibration::default();
        for (pace, workers, read, write) in [
            (Pace::Gentle, 2, Some(100), Some(1)),
            (Pace::Default, 4, Some(200), Some(1)),
            (Pace::Unthrottled, 8, None, None),
        ] {
            let view = resolved(&set, &yours, &unmeasured, pace);
            let host = &view.host;
            let sets = &view.roots[0];
            assert_eq!(
                [
                    host.concurrent.in_force,
                    host.hashers.in_force,
                    host.archive_workers.in_force,
                    sets.readers.in_force
                ],
                [Some(workers); 4],
                "{pace:?}"
            );
            assert_eq!(sets.read_mib_per_s.in_force, read, "{pace:?}");
            assert_eq!(sets.write_mib_per_s.in_force, write, "{pace:?}");
        }
    }

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("store")).unwrap();
        (dir, store)
    }

    /// Registers a read-only Root `name` in a new folder of `dir`, with the Operator's Ceilings `ceilings`.
    fn register(store: &Store, dir: &tempfile::TempDir, name: &str, ceilings: [Option<u64>; 3]) {
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        let [readers, read_mib_per_s, write_mib_per_s] = ceilings;
        store
            .register_root(
                &stage_select_api::roots::RootSpec {
                    name: name.into(),
                    path: path.display().to_string(),
                    readers,
                    read_mib_per_s,
                    write_mib_per_s,
                    ..Default::default()
                },
                "test",
            )
            .unwrap();
    }

    fn update(ceilings: CeilingsUpdate) -> SettingsUpdate {
        SettingsUpdate {
            ceilings: Some(ceilings),
            ..SettingsUpdate::default()
        }
    }

    fn refused(store: &Store, ceilings: CeilingsUpdate) -> String {
        match store.update_settings(&update(ceilings), "Operator") {
            Err(Error::Refused(why)) => why,
            other => panic!("not refused: {other:?}"),
        }
    }

    #[test]
    fn a_ceiling_out_of_range_or_for_no_root_is_refused_and_changes_nothing() {
        let (dir, store) = store();
        register(&store, &dir, "sets", [None; 3]);
        let host = |h: HostCeilingsUpdate| CeilingsUpdate {
            host: h,
            ..CeilingsUpdate::default()
        };
        let root = |r: RootCeilingsUpdate| CeilingsUpdate {
            roots: BTreeMap::from([("sets".to_owned(), r)]),
            ..CeilingsUpdate::default()
        };
        for bad in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("4"),
            serde_json::json!(5000),
        ] {
            let why = refused(
                &store,
                host(HostCeilingsUpdate {
                    concurrent: Some(bad.clone()),
                    ..HostCeilingsUpdate::default()
                }),
            );
            assert_eq!(
                why,
                "Set the most jobs at once to a whole number from 1 to 1024, or leave it empty to use the default.",
                "{bad}"
            );
        }
        let why = refused(
            &store,
            root(RootCeilingsUpdate {
                read_mib_per_s: Some(serde_json::json!(0)),
                ..RootCeilingsUpdate::default()
            }),
        );
        assert!(why.contains("the read rate of sets"), "{why}");
        let why = refused(
            &store,
            CeilingsUpdate {
                roots: BTreeMap::from([("elsewhere".to_owned(), RootCeilingsUpdate::default())]),
                ..CeilingsUpdate::default()
            },
        );
        assert_eq!(why, "There is no root elsewhere.");
        assert_eq!(store.settings().unwrap().ceilings, HostOverrides::default());
        assert_eq!(
            store.roots().root("sets").unwrap().overrides(),
            RootOverrides::default()
        );
    }

    #[test]
    fn your_ceilings_replace_those_you_set_before_and_reach_the_shared_pools() {
        let (dir, store) = store();
        register(&store, &dir, "sets", [None; 3]);
        let pools = store.job_pools("sets").unwrap();
        let set = CeilingsUpdate {
            host: HostCeilingsUpdate {
                hashers: Some(6.into()),
                archive_workers: Some(8.into()),
                concurrent: Some(serde_json::Value::Null),
                ..HostCeilingsUpdate::default()
            },
            roots: BTreeMap::from([(
                "sets".to_owned(),
                RootCeilingsUpdate {
                    readers: Some(10.into()),
                    write_mib_per_s: Some(80.into()),
                    ..RootCeilingsUpdate::default()
                },
            )]),
        };
        store.update_settings(&update(set), "Operator").unwrap();
        // The default pace: half of each.
        let p = pools.pools();
        assert_eq!((p.readers.limit(), p.hashers.limit()), (5, 3));
        assert_eq!(store.archive_gate().permits(), 4);
        let view = store.settings_view().unwrap().jobs;
        assert_eq!(view.roots[0].write_mib_per_s.in_force, Some(40));
        assert_eq!(
            store.roots().root("sets").unwrap().readers,
            Some(10),
            "kept with the root"
        );

        // A change of pace reaches the same pools.
        let gentle = SettingsUpdate {
            pace: Some(Pace::Gentle),
            ..SettingsUpdate::default()
        };
        store.update_settings(&gentle, "Operator").unwrap();
        assert_eq!((p.readers.limit(), p.hashers.limit()), (2, 1));
        assert_eq!(store.archive_gate().permits(), 2);

        // Setting the Host's Ceilings again replaces them: those left out fall back. A root left out keeps its own.
        let only = CeilingsUpdate {
            host: HostCeilingsUpdate {
                hashers: Some(12.into()),
                ..HostCeilingsUpdate::default()
            },
            ..CeilingsUpdate::default()
        };
        store.update_settings(&update(only), "Operator").unwrap();
        let yours = store.settings().unwrap().ceilings;
        assert_eq!(yours.hashers, Some(12));
        assert_eq!(yours.archive_workers, None);
        assert_eq!(store.roots().root("sets").unwrap().readers, Some(10));
        assert_eq!(p.hashers.limit(), 3);
        assert_eq!(
            store.archive_gate().permits(),
            1,
            "a quarter of the default 2, at least 1"
        );

        // Clearing a root's Ceilings in the update clears them.
        let cleared = CeilingsUpdate {
            roots: BTreeMap::from([("sets".to_owned(), RootCeilingsUpdate::default())]),
            ..CeilingsUpdate::default()
        };
        store.update_settings(&update(cleared), "Operator").unwrap();
        assert_eq!(
            store.roots().root("sets").unwrap().overrides(),
            RootOverrides::default()
        );
    }

    #[test]
    fn a_ceiling_you_set_overrides_the_calibration() {
        let (dir, store) = store();
        register(&store, &dir, "sets", [None; 3]);
        store
            .write(|tx| {
                tx.execute(
                    "INSERT INTO jobs (state, kind, actor, readers, hashers, created_at)
                     VALUES ('succeeded', 'calibration', 'Operator', 1, 1, 'then')",
                    [],
                )?;
                let job = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO calibration_roots (job_id, root, path, outcome, available_bytes, curve, knee,
                                                    knee_bytes_per_s, measured_at)
                     VALUES (?1, 'sets', ?2, 'measured', 1, '[]', 3, ?3, '2026-01-01T00:00:00Z')",
                    rusqlite::params![
                        job,
                        dir.path().join("sets").display().to_string(),
                        250.0 * MIB
                    ],
                )?;
                Ok(())
            })
            .unwrap();
        store.refresh_ceilings().unwrap();
        let sets = |store: &Store| store.ceilings().unwrap().roots[0].clone();
        assert_eq!(
            of(&sets(&store).readers),
            (Some(3), Some(CeilingSource::Calibration))
        );
        assert_eq!(
            of(&sets(&store).read_mib_per_s),
            (Some(250), Some(CeilingSource::Calibration))
        );

        let yours = CeilingsUpdate {
            roots: BTreeMap::from([(
                "sets".to_owned(),
                RootCeilingsUpdate {
                    readers: Some(5.into()),
                    read_mib_per_s: Some(90.into()),
                    ..RootCeilingsUpdate::default()
                },
            )]),
            ..CeilingsUpdate::default()
        };
        store.update_settings(&update(yours), "Operator").unwrap();
        let after = sets(&store);
        assert_eq!(of(&after.readers), (Some(5), Some(CeilingSource::Yours)));
        assert_eq!(after.readers.measured, Some(3));
        assert_eq!(
            of(&after.read_mib_per_s),
            (Some(90), Some(CeilingSource::Yours))
        );
    }

    #[test]
    fn the_pace_and_your_ceilings_set_each_roots_byte_caps_live() {
        let (dir, store) = store();
        register(&store, &dir, "sets", [None, Some(400), None]);
        register(&store, &dir, "library", [None; 3]);
        let meter_caps = RootLimits::caps(&*store.ceilings.readers, "sets").unwrap();
        store.refresh_ceilings().unwrap();
        let mib = 1u64 << 20;
        let caps = |root: &str| store.byte_caps().get(root).copied().unwrap_or_default();
        let read = |c: ByteCaps| c.read_bytes_per_s;
        let write = |c: ByteCaps| c.write_bytes_per_s;
        assert_eq!(
            read(caps("sets")),
            Some(200 * mib),
            "half, at the default pace"
        );
        assert_eq!(write(caps("sets")), None, "no write cap unless you set one");
        assert_eq!(caps("library"), ByteCaps::default(), "no byte-rate ceiling");
        assert_eq!(
            meter_caps.read.rate(),
            Some(200 * mib),
            "the Jobs' own bucket"
        );

        for (pace, cap) in [
            (Pace::Gentle, Some(100 * mib)),
            (Pace::Unthrottled, None),
            (Pace::Default, Some(200 * mib)),
        ] {
            let update = SettingsUpdate {
                pace: Some(pace),
                ..SettingsUpdate::default()
            };
            store.update_settings(&update, "Operator").unwrap();
            assert_eq!(read(caps("sets")), cap, "{pace:?}");
            assert_eq!(meter_caps.read.rate(), cap, "{pace:?}");
        }

        let yours = CeilingsUpdate {
            roots: BTreeMap::from([(
                "library".to_owned(),
                RootCeilingsUpdate {
                    write_mib_per_s: Some(80.into()),
                    ..RootCeilingsUpdate::default()
                },
            )]),
            ..CeilingsUpdate::default()
        };
        store.update_settings(&update(yours), "Operator").unwrap();
        assert_eq!(write(caps("library")), Some(40 * mib));
        assert_eq!(read(caps("library")), None);
    }

    /// Counts the files being read at once of one Root, by every Job reading it, and the count each file started at,
    /// by phase. Each phase but the first begins with a change, made once as many files as it says have ended: a file
    /// starting after that waits for the change. The first files of a phase may meet, each waiting for the others to
    /// start, so that that many are surely read at once.
    #[derive(Default)]
    struct Reading {
        now: AtomicUsize,
        most: AtomicUsize,
        ended: AtomicUsize,
        /// When each change after the first phase is due: once this many files have ended.
        due: Vec<usize>,
        phase: Mutex<Phase>,
        changed: std::sync::Condvar,
        /// Each file's phase, and how many were being read as it started.
        starts: Mutex<Vec<(usize, usize)>>,
    }

    #[derive(Default)]
    struct Phase {
        now: usize,
        meeting: Option<Arc<std::sync::Barrier>>,
        to_meet: usize,
    }

    impl Phase {
        fn meeting(now: usize, meeting: usize) -> Phase {
            Phase {
                now,
                meeting: Some(Arc::new(std::sync::Barrier::new(meeting))),
                to_meet: meeting,
            }
        }
    }

    impl Reading {
        /// Changes due as `due` says, and a first phase whose first `meeting` files meet.
        fn new(due: Vec<usize>, meeting: usize) -> Reading {
            Reading {
                due,
                phase: Mutex::new(Phase::meeting(0, meeting)),
                ..Reading::default()
            }
        }

        /// Begins the next phase, whose first `meeting` files meet.
        fn change(&self, meeting: usize) {
            let mut phase = self.phase.lock().unwrap();
            *phase = Phase::meeting(phase.now + 1, meeting);
            self.changed.notify_all();
        }

        fn start(&self) {
            let ended = self.ended.load(Ordering::SeqCst);
            let mut phase = self.phase.lock().unwrap();
            while self.due.get(phase.now).is_some_and(|&due| ended >= due) {
                phase = self.changed.wait(phase).unwrap();
            }
            let meeting = (phase.to_meet > 0).then(|| {
                phase.to_meet -= 1;
                phase.meeting.clone()
            });
            let at = phase.now;
            drop(phase);
            let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(now, Ordering::SeqCst);
            self.starts.lock().unwrap().push((at, now));
            if let Some(Some(meeting)) = meeting {
                meeting.wait();
            }
        }

        fn end(&self) {
            self.now.fetch_sub(1, Ordering::SeqCst);
            self.ended.fetch_add(1, Ordering::SeqCst);
        }

        /// The most read at once as each file of phase `phase` started, after its first `skip`.
        fn most_started(&self, phase: usize, skip: usize) -> usize {
            let starts = self.starts.lock().unwrap();
            let of: Vec<usize> = starts
                .iter()
                .filter(|(at, _)| *at == phase)
                .map(|(_, now)| *now)
                .collect();
            assert!(of.len() > skip, "phase {phase}: {starts:?}");
            of[skip..].iter().copied().max().unwrap()
        }
    }

    /// An Audit's reads, counted as they start and end.
    impl stage_select_engine::PayloadReads for Reading {
        fn prior(&self, _: &str) -> Option<stage_select_engine::PayloadRead> {
            self.start();
            None
        }

        fn record(&self, _: &str, _: &stage_select_engine::PayloadRead) {
            self.end();
        }
    }

    /// A file that counts while it is read, from its first read until it drops.
    struct Counted<'r> {
        file: std::fs::File,
        reading: &'r Reading,
        started: bool,
    }

    impl std::io::Read for Counted<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if !self.started {
                self.started = true;
                self.reading.start();
            }
            self.file.read(buf)
        }
    }

    impl Drop for Counted<'_> {
        fn drop(&mut self) {
            if self.started {
                self.reading.end();
            }
        }
    }

    /// A Source Set of `n` files, none of which the DAT names, and the DAT.
    fn files(n: usize) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let set = dir.path().join("set");
        std::fs::create_dir(&set).unwrap();
        for i in 0..n {
            std::fs::write(set.join(format!("{i}.bin")), vec![i as u8; 4096]).unwrap();
        }
        let dat = dir.path().join("Test.dat");
        std::fs::write(
            &dat,
            "<datafile><header><name>Test</name><version>1</version></header></datafile>",
        )
        .unwrap();
        (dir, set, dat)
    }

    fn audit(
        (set, dat): (&std::path::Path, &std::path::Path),
        workers: stage_select_api::Workers,
        pools: &JobPools,
        tally: stage_select_engine::meter::Tally,
        reads: &Reading,
        progress: &(dyn Fn(stage_select_engine::Progress) + Sync),
    ) {
        let options = stage_select_engine::Options {
            file: "Test.dat".into(),
            confine: None,
            workers,
            pools: Some(pools.pools()),
            progress: Some(progress),
            evidence: stage_select_api::report::AuditEvidence::Full,
            reads: Some(reads),
            tally,
            placed: &[],
            disks: None,
        };
        stage_select_engine::audit_with(dat, set, &options).unwrap();
    }

    /// Runs `f` on a thread of its own, failing if it takes longer than 60 s.
    fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(60))
            .expect("it finished in time: no deadlock, and no permit stuck")
    }

    fn readers_of_sets(readers: u64) -> SettingsUpdate {
        update(CeilingsUpdate {
            roots: BTreeMap::from([(
                "sets".to_owned(),
                RootCeilingsUpdate {
                    readers: Some(readers.into()),
                    ..RootCeilingsUpdate::default()
                },
            )]),
            ..CeilingsUpdate::default()
        })
    }

    #[test]
    fn a_running_job_reads_more_at_once_when_a_ceiling_or_the_pace_rises_and_fewer_when_it_falls() {
        bounded(|| {
            let (dir, store) = store();
            register(&store, &dir, "sets", [None; 3]);
            store
                .update_settings(&readers_of_sets(4), "Operator")
                .unwrap();
            let workers = store.job_workers();
            let pools = store.job_pools("sets").unwrap();
            let tally = store.meter().start(1, workers).tally(Some("sets"));
            let (_files, set, dat) = files(120);
            let reads = Reading::new(vec![20, 50, 80], 2);
            // Two at once at first, the default pace's half of your 4; six once you raise it to 12; one once you
            // lower it to 1, even unthrottled; then three once the pace is gentle and you raise it to 12 again. Each
            // change is made as that many files are done, and the files after it wait for it.
            let progress = |p: stage_select_engine::Progress| {
                let set = |u: SettingsUpdate| {
                    store.update_settings(&u, "Operator").unwrap();
                };
                match p.files_done {
                    20 => {
                        set(readers_of_sets(12));
                        reads.change(6);
                    }
                    50 => {
                        set(readers_of_sets(1));
                        set(SettingsUpdate {
                            pace: Some(Pace::Unthrottled),
                            ..SettingsUpdate::default()
                        });
                        reads.change(0);
                    }
                    80 => {
                        set(SettingsUpdate {
                            pace: Some(Pace::Gentle),
                            ..SettingsUpdate::default()
                        });
                        set(readers_of_sets(12));
                        reads.change(3);
                    }
                    _ => {}
                }
            };
            audit((&set, &dat), workers, &pools, tally, &reads, &progress);

            // The files under way at each change finish as they started, up to six of them as the readers fall to one;
            // every file started after them follows it.
            assert_eq!(reads.most_started(0, 0), 2);
            assert_eq!(reads.most_started(1, 0), 6);
            assert_eq!(reads.most_started(2, 6), 1);
            assert_eq!(reads.most_started(3, 0), 3);
        });
    }

    #[test]
    fn a_build_reading_a_root_and_an_audit_of_it_share_its_readers() {
        bounded(|| {
            let (dir, store) = store();
            register(&store, &dir, "sets", [None; 3]);
            store
                .update_settings(&readers_of_sets(4), "Operator")
                .unwrap();
            let pools = store.job_pools("sets").unwrap();
            let shared = pools.pools().readers;
            assert_eq!(shared.limit(), 2);
            let (_files, set, dat) = files(40);
            // The first two files to start, of either Job, meet: two are surely read at once.
            let reads = Reading::new(Vec::new(), 2);

            // The Root's readers all taken by other Jobs: a Build's read of the Root waits for one.
            let build = store.meter().start(2, store.job_workers());
            let held = (shared.enter(), shared.enter());
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::scope(|s| {
                let tally = build.tally(Some("sets"));
                let path = set.join("0.bin");
                s.spawn(move || {
                    let file = std::fs::File::open(path).unwrap();
                    tx.send(crate::builds::sha256_of(&file, &tally).unwrap())
                        .unwrap();
                });
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                while shared.waiting() == 0 {
                    assert!(std::time::Instant::now() < deadline, "it never waited");
                    std::thread::yield_now();
                }
                assert!(
                    rx.try_recv().is_err(),
                    "it read while every reader was taken"
                );
                drop(held);
                rx.recv_timeout(std::time::Duration::from_secs(30))
                    .expect("it reads once a reader is free");
            });

            // A Build reading every file of the Root alongside an Audit of it: never more than two at once in all.
            let audited = store.meter().start(1, store.job_workers());
            std::thread::scope(|s| {
                s.spawn(|| {
                    let tally = audited.tally(Some("sets"));
                    let workers = store.job_workers();
                    audit((&set, &dat), workers, &pools, tally, &reads, &|_| {});
                });
                for _ in 0..4 {
                    s.spawn(|| {
                        let tally = build.tally(Some("sets"));
                        for i in 0..10 {
                            let file = std::fs::File::open(set.join(format!("{i}.bin"))).unwrap();
                            let counted = Counted {
                                file,
                                reading: &reads,
                                started: false,
                            };
                            let mut reading =
                                stage_select_engine::meter::Reading::new(counted, &tally);
                            std::io::copy(&mut reading, &mut std::io::sink()).unwrap();
                        }
                    });
                }
            });
            assert_eq!(reads.most.load(Ordering::SeqCst), 2);
            assert_eq!(
                reads.starts.lock().unwrap().len(),
                80,
                "40 audited, 40 built"
            );
            assert_eq!(shared.taken(), 0);
        });
    }
}
