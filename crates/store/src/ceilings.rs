//! The Ceilings of the Host and each Root (ADR 0022): where each comes from, what the pace allows of it now, and the
//! pools every Job shares to keep to it.
//!
//! Each Ceiling takes the first of these that has a value: the configuration file, which locks it; the Operator's
//! value, saved in the `ceilings` settings section; the latest Calibration's knee; the CPU count. Byte rates have no
//! CPU-count fallback, and 7z archive workers, which aren't measured, fall back to a fixed default.
//!
//! The resolved Ceilings are worked out again when the settings or a Calibration change them, never while a Job
//! reads: each worker takes a permit from a pool whose size is already the pace's share, at its next file.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use rusqlite::Connection;
use title_select_api::calibration::Calibration;
use title_select_api::settings::{
    Ceiling, CeilingInput, CeilingOverrides, CeilingSource, CeilingsUpdate,
    DEFAULT_ARCHIVE_WORKERS, HostCeilings, HostOverrides, JobsView, MAX_ARCHIVE_WORKERS,
    MAX_MIB_PER_S, MAX_WORKERS, Pace, RootCeilings, RootOverrides,
};
use title_select_engine::Pools;
use title_select_engine::pool::Pool;
use title_select_engine::sevenz::Gate;

use crate::config::Config;
use crate::{Error, Result, Store};

const MIB: f64 = 1024.0 * 1024.0;

/// What the Ceilings are worked out from, besides the configuration file.
pub struct Sources<'a> {
    /// The Operator's values, saved in the workbench.
    pub yours: &'a CeilingOverrides,
    pub calibration: &'a Calibration,
    /// The CPUs the Service can use.
    pub cpus: usize,
    pub pace: Pace,
    /// The fastest write throughput past Jobs averaged into each Root, in MiB/s.
    pub observed_writes: &'a BTreeMap<String, u64>,
}

/// One Ceiling from its sources, first first: the configuration file's value and the key that sets it, the
/// Operator's, the measured one, then the fallback. `in_force` is the pace's share of it.
fn ceiling(
    config: Option<(u64, String)>,
    yours: Option<u64>,
    measured: Option<u64>,
    fallback: Option<(u64, CeilingSource)>,
    in_force: impl Fn(u64) -> Option<u64>,
) -> Ceiling {
    let (value, source) = match (&config, yours, measured, fallback) {
        (Some((value, _)), ..) => (Some(*value), Some(CeilingSource::Config)),
        (None, Some(value), ..) => (Some(value), Some(CeilingSource::Yours)),
        (None, None, Some(value), _) => (Some(value), Some(CeilingSource::Calibration)),
        (None, None, None, Some((value, source))) => (Some(value), Some(source)),
        (None, None, None, None) => (None, None),
    };
    Ceiling {
        ceiling: value,
        in_force: value.and_then(in_force),
        source,
        yours,
        measured,
        locked_by: config.map(|(_, key)| key),
    }
}

/// Every Ceiling of the Host and of each Root `config` names, and what `sources.pace` allows of each.
pub fn resolve(config: &Config, sources: &Sources) -> JobsView {
    let pace = sources.pace;
    let workers = |c: u64| Some(pace.of(c as usize) as u64);
    let rate = |c: u64| pace.rate(Some(c));
    let cpus = Some((sources.cpus.max(1) as u64, CeilingSource::Cpus));
    let set = |value: Option<usize>, key: &str| value.map(|v| (v as u64, key.to_owned()));
    let yours = &sources.yours.host;
    let host_knee = sources
        .calibration
        .host
        .as_ref()
        .and_then(|h| h.knee)
        .map(|k| k.workers as u64);
    let host = HostCeilings {
        concurrent: ceiling(
            set(config.jobs.concurrent, "[jobs] concurrent"),
            yours.concurrent,
            None,
            cpus,
            workers,
        ),
        hashers: ceiling(
            set(config.jobs.hashers, "[jobs] hashers"),
            yours.hashers,
            host_knee,
            cpus,
            workers,
        ),
        archive_workers: ceiling(
            set(config.ingest.archive_workers, "[ingest] archive_workers"),
            yours.archive_workers,
            None,
            Some((DEFAULT_ARCHIVE_WORKERS, CeilingSource::Default)),
            workers,
        ),
    };
    let roots = config
        .roots
        .iter()
        .map(|(name, root)| {
            let yours = sources.yours.roots.get(name).cloned().unwrap_or_default();
            let knee = sources
                .calibration
                .roots
                .iter()
                .find(|r| &r.root == name)
                .and_then(|r| r.knee);
            let readers_set = root
                .readers
                .map(|v| (v, format!("[roots.{name}] readers")))
                .or_else(|| set(config.jobs.readers, "[jobs] readers"));
            let rate_set =
                |value: Option<u64>, key: &str| value.map(|v| (v, format!("[roots.{name}] {key}")));
            RootCeilings {
                root: name.clone(),
                readers: ceiling(
                    readers_set,
                    yours.readers,
                    knee.map(|k| k.workers as u64),
                    cpus,
                    workers,
                ),
                read_mib_per_s: ceiling(
                    rate_set(root.read_mib_per_s, "read_mib_per_s"),
                    yours.read_mib_per_s,
                    knee.map(|k| ((k.bytes_per_s / MIB) as u64).max(1)),
                    None,
                    rate,
                ),
                write_mib_per_s: ceiling(
                    rate_set(root.write_mib_per_s, "write_mib_per_s"),
                    yours.write_mib_per_s,
                    None,
                    None,
                    rate,
                ),
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

/// Checks `update` against `config` and returns the Operator's Ceilings it sets: each a whole number in range, for
/// a Root `config` names, and none the configuration file sets.
pub(crate) fn checked(config: &Config, update: &CeilingsUpdate) -> Result<CeilingOverrides> {
    let host = &update.host;
    let host = HostOverrides {
        concurrent: value(
            &host.concurrent,
            "the most jobs at once",
            MAX_WORKERS,
            config.jobs.concurrent.map(|_| "[jobs] concurrent"),
        )?,
        hashers: value(
            &host.hashers,
            "the hash workers",
            MAX_WORKERS,
            config.jobs.hashers.map(|_| "[jobs] hashers"),
        )?,
        archive_workers: value(
            &host.archive_workers,
            "the 7z archive workers",
            MAX_ARCHIVE_WORKERS,
            config
                .ingest
                .archive_workers
                .map(|_| "[ingest] archive_workers"),
        )?,
    };
    let mut roots = BTreeMap::new();
    for (name, update) in &update.roots {
        let Some(root) = config.root(name) else {
            return Err(Error::Refused(format!(
                "{name} is not a root in your configuration."
            )));
        };
        let readers_key = match (root.readers, config.jobs.readers) {
            (Some(_), _) => Some(format!("[roots.{name}] readers")),
            (None, Some(_)) => Some("[jobs] readers".to_owned()),
            (None, None) => None,
        };
        let key = |set: Option<u64>, key: &str| set.map(|_| format!("[roots.{name}] {key}"));
        let overrides = RootOverrides {
            readers: value(
                &update.readers,
                &format!("the readers on {name}"),
                MAX_WORKERS,
                readers_key.as_deref(),
            )?,
            read_mib_per_s: value(
                &update.read_mib_per_s,
                &format!("the read rate of {name}"),
                MAX_MIB_PER_S,
                key(root.read_mib_per_s, "read_mib_per_s").as_deref(),
            )?,
            write_mib_per_s: value(
                &update.write_mib_per_s,
                &format!("the write rate of {name}"),
                MAX_MIB_PER_S,
                key(root.write_mib_per_s, "write_mib_per_s").as_deref(),
            )?,
        };
        if !overrides.is_empty() {
            roots.insert(name.clone(), overrides);
        }
    }
    Ok(CeilingOverrides { host, roots })
}

/// One Ceiling as given: none when null or absent, else a whole number from 1 to `max` that `locked_by`, the
/// configuration key setting it if any, doesn't lock.
fn value(
    given: &CeilingInput,
    what: &str,
    max: u64,
    locked_by: Option<&str>,
) -> Result<Option<u64>> {
    let given = match given {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(given) => given,
    };
    if let Some(key) = locked_by {
        return Err(Error::Refused(format!(
            "Your configuration file sets {what} ({key}), so you can't change it here. Change it in the file, \
             then restart the service."
        )));
    }
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
    readers: RwLock<BTreeMap<String, Arc<Pool>>>,
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
            readers: RwLock::default(),
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
            let mut readers = self.readers.write().unwrap_or_else(|e| e.into_inner());
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
        *self.view.write().unwrap_or_else(|e| e.into_inner()) = Some(view);
        self.ready.store(true, Ordering::Release);
    }
}

/// A Job's pools: its Root's readers and the Host's hash workers.
pub struct JobPools {
    readers: Arc<Pool>,
    hashers: Arc<Pool>,
}

impl JobPools {
    /// Lets `meter` report this Job's allowance from these pools.
    pub fn meter(&self, meter: &title_select_engine::meter::JobMeter) {
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
    /// Works the Ceilings out again under `config`, from the settings, the latest Calibration and past Jobs' writes,
    /// and applies them to every Job from its workers' next file. Run when any of those change.
    pub fn refresh_ceilings(&self, config: &Config) -> Result<JobsView> {
        let _one_at_a_time = self
            .ceilings
            .refreshing
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let cpus = crate::calibration::cpus();
        let calibration = self.calibration(config, cpus)?;
        let (yours, observed_writes) =
            self.read(|c| Ok((crate::settings::load(c)?.ceilings, observed_writes(c)?)))?;
        let view = resolve(
            config,
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

    /// The Ceilings in force: worked out under `config` when first asked for, and again whenever they change.
    pub fn ceilings(&self, config: &Config) -> Result<JobsView> {
        self.live_ceilings(config)?;
        Ok(self
            .ceilings
            .view
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .expect("ceilings are worked out once ready"))
    }

    fn live_ceilings(&self, config: &Config) -> Result<&Live> {
        if !self.ceilings.ready.load(Ordering::Acquire) {
            self.refresh_ceilings(config)?;
        }
        Ok(&self.ceilings)
    }

    /// The threads a Job queued now starts: as many readers as the largest reader Ceiling of any Root, and as many
    /// hash workers as the Host's Ceiling. The shared pools say how many of them work at each moment; a Ceiling
    /// raised later reaches a Job queued before only as far as its threads. The CPU count of each when the Ceilings
    /// can't be worked out.
    pub fn job_workers(&self, config: &Config) -> title_select_api::Workers {
        let cpus = crate::calibration::cpus();
        let Ok(view) = self.ceilings(config) else {
            return title_select_api::Workers {
                readers: cpus,
                hashers: cpus,
            };
        };
        let ceiling = |c: &Ceiling| c.ceiling.unwrap_or(1) as usize;
        title_select_api::Workers {
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
    pub fn concurrent_in_force(&self, config: &Config) -> Result<usize> {
        Ok(self
            .live_ceilings(config)?
            .concurrent
            .load(Ordering::SeqCst))
    }

    /// The pools a Job reading the Root `root` shares with every other Job.
    pub fn job_pools(&self, config: &Config, root: &str) -> Result<JobPools> {
        let live = self.live_ceilings(config)?;
        let found = live
            .readers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(root)
            .cloned();
        // A Root the configuration doesn't name is read with the readers of the CPU count, at this pace.
        let readers = found.unwrap_or_else(|| {
            let limit = self.pace().of(crate::calibration::cpus());
            live.readers
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .entry(root.to_owned())
                .or_insert_with(|| Arc::new(Pool::new(limit)))
                .clone()
        });
        Ok(JobPools {
            readers,
            hashers: live.hashers.clone(),
        })
    }

    /// The threads chdman compresses with: the pace's share of the hash workers Ceiling, or chdman's own choice of
    /// every core when `unthrottled`.
    pub(crate) fn chdman_threads(&self, config: &Config) -> Result<Option<usize>> {
        let view = self.ceilings(config)?;
        let hashers = view.host.hashers.ceiling.unwrap_or(1) as usize;
        Ok(view.pace.chdman_threads(hashers))
    }

    /// The gate every 7z archive decodes through, its size the pace's share of the archive workers Ceiling.
    pub(crate) fn archive_gate(&self, config: &Config) -> &Gate {
        match self.live_ceilings(config) {
            Ok(live) => &live.archives,
            // The Ceilings can't be worked out: the gate keeps the size it had, one archive at a time at first.
            Err(_) => &self.ceilings.archives,
        }
    }
}

#[cfg(test)]
mod tests {
    use title_select_api::calibration::{HostCalibration, Knee, RootCalibration, RootOutcome};
    use title_select_api::settings::{HostCeilingsUpdate, RootCeilingsUpdate, SettingsUpdate};

    use super::*;

    fn config(text: &str) -> Config {
        Config::parse(text).unwrap()
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
        config: &Config,
        yours: &CeilingOverrides,
        calibration: &Calibration,
        pace: Pace,
    ) -> JobsView {
        resolve(
            config,
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
                         SELECT 'Set', 'sets', 'Set', id, 'then' FROM dats;
                     -- Only the totals matter here, not the repair plan a repair Job needs.
                     PRAGMA ignore_check_constraints = ON;",
                )?;
                for (kind, written, ms) in [
                    ("repair", 100i64 << 20, 2_000i64),
                    ("repair", 300 << 20, 2_000),
                    ("repair", 0, 1_000),
                    ("audit", 900 << 20, 1_000),
                ] {
                    tx.execute(
                        "INSERT INTO jobs (state, kind, source_set_id, dat_revision_id, actor, readers, hashers,
                                           created_at, bytes_written, duration_ms)
                         SELECT 'succeeded', ?1, s.id, r.id, 'Operator', 1, 1, 'then', ?2, ?3
                         FROM source_sets s, dat_revisions r",
                        rusqlite::params![kind, written, ms],
                    )?;
                }
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
    fn each_ceiling_takes_the_file_then_yours_then_the_calibration_then_the_cpu_count() {
        use CeilingSource as S;
        let bare = config("[roots.sets]\npath = \"/sets\"");
        let none = CeilingOverrides::default();
        let unmeasured = Calibration::default();
        let view = resolved(&bare, &none, &unmeasured, Pace::Unthrottled);
        assert_eq!(of(&view.host.concurrent), (Some(8), Some(S::Cpus)));
        assert_eq!(of(&view.host.hashers), (Some(8), Some(S::Cpus)));
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

        let yours = CeilingOverrides {
            host: HostOverrides {
                concurrent: Some(3),
                hashers: Some(5),
                archive_workers: Some(4),
            },
            roots: BTreeMap::from([(
                "sets".to_owned(),
                RootOverrides {
                    readers: Some(6),
                    read_mib_per_s: Some(100),
                    write_mib_per_s: Some(40),
                },
            )]),
        };
        let view = resolved(&bare, &yours, &measured, Pace::Unthrottled);
        assert_eq!(of(&view.host.hashers), (Some(5), Some(S::Yours)));
        assert_eq!(view.host.hashers.measured, Some(3));
        assert_eq!(of(&view.host.archive_workers), (Some(4), Some(S::Yours)));
        let sets = &view.roots[0];
        assert_eq!(of(&sets.readers), (Some(6), Some(S::Yours)));
        assert_eq!(of(&sets.read_mib_per_s), (Some(100), Some(S::Yours)));
        assert_eq!(of(&sets.write_mib_per_s), (Some(40), Some(S::Yours)));

        let file = config(
            "[jobs]\nconcurrent = 2\nhashers = 7\n[ingest]\narchive_workers = 1\n\
             [roots.sets]\npath = \"/sets\"\nreaders = 9\nread_mib_per_s = 300\nwrite_mib_per_s = 50",
        );
        let view = resolved(&file, &yours, &measured, Pace::Unthrottled);
        assert_eq!(of(&view.host.concurrent), (Some(2), Some(S::Config)));
        assert_eq!(of(&view.host.hashers), (Some(7), Some(S::Config)));
        assert_eq!(
            view.host.hashers.locked_by.as_deref(),
            Some("[jobs] hashers")
        );
        assert_eq!(
            view.host.hashers.yours,
            Some(5),
            "kept, though it doesn't apply"
        );
        assert_eq!(of(&view.host.archive_workers), (Some(1), Some(S::Config)));
        let sets = &view.roots[0];
        assert_eq!(of(&sets.readers), (Some(9), Some(S::Config)));
        assert_eq!(
            sets.readers.locked_by.as_deref(),
            Some("[roots.sets] readers")
        );
        assert_eq!(of(&sets.read_mib_per_s), (Some(300), Some(S::Config)));
        assert_eq!(of(&sets.write_mib_per_s), (Some(50), Some(S::Config)));
    }

    #[test]
    fn jobs_readers_in_the_file_is_the_reader_ceiling_of_every_root_without_its_own() {
        let file = config(
            "[jobs]\nreaders = 3\n[roots.a]\npath = \"/a\"\n[roots.b]\npath = \"/b\"\nreaders = 5",
        );
        let view = resolved(
            &file,
            &CeilingOverrides::default(),
            &Calibration::default(),
            Pace::Unthrottled,
        );
        let readers: Vec<_> = view
            .roots
            .iter()
            .map(|r| (r.readers.ceiling, r.readers.locked_by.clone()))
            .collect();
        assert_eq!(
            readers,
            [
                (Some(3), Some("[jobs] readers".to_owned())),
                (Some(5), Some("[roots.b] readers".to_owned()))
            ]
        );
    }

    #[test]
    fn the_pace_scales_every_ceiling_and_unthrottled_sets_no_byte_cap() {
        let file = config(
            "[jobs]\nconcurrent = 8\nhashers = 8\n[ingest]\narchive_workers = 8\n\
             [roots.sets]\npath = \"/sets\"\nreaders = 8\nread_mib_per_s = 400\nwrite_mib_per_s = 3",
        );
        let none = CeilingOverrides::default();
        let unmeasured = Calibration::default();
        for (pace, workers, read, write) in [
            (Pace::Gentle, 2, Some(100), Some(1)),
            (Pace::Default, 4, Some(200), Some(1)),
            (Pace::Unthrottled, 8, None, None),
        ] {
            let view = resolved(&file, &none, &unmeasured, pace);
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
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn update(ceilings: CeilingsUpdate) -> SettingsUpdate {
        SettingsUpdate {
            ceilings: Some(ceilings),
            ..SettingsUpdate::default()
        }
    }

    fn refused(store: &Store, config: &Config, ceilings: CeilingsUpdate) -> String {
        match store.update_settings(config, &update(ceilings), "Operator") {
            Err(Error::Refused(why)) => why,
            other => panic!("not refused: {other:?}"),
        }
    }

    #[test]
    fn a_ceiling_the_file_sets_or_one_out_of_range_is_refused_and_changes_nothing() {
        let (_dir, store) = store();
        let config = config(
            "[jobs]\nhashers = 4\nreaders = 2\n[ingest]\narchive_workers = 3\n\
             [roots.sets]\npath = \"/sets\"\nwrite_mib_per_s = 10",
        );
        let host = |h: HostCeilingsUpdate| CeilingsUpdate {
            host: h,
            ..CeilingsUpdate::default()
        };
        let root = |r: RootCeilingsUpdate| CeilingsUpdate {
            roots: BTreeMap::from([("sets".to_owned(), r)]),
            ..CeilingsUpdate::default()
        };
        let why = refused(
            &store,
            &config,
            host(HostCeilingsUpdate {
                hashers: Some(6.into()),
                ..HostCeilingsUpdate::default()
            }),
        );
        assert!(
            why.contains("configuration file sets the hash workers ([jobs] hashers)"),
            "{why}"
        );
        let why = refused(
            &store,
            &config,
            host(HostCeilingsUpdate {
                archive_workers: Some(1.into()),
                ..HostCeilingsUpdate::default()
            }),
        );
        assert!(why.contains("[ingest] archive_workers"), "{why}");
        let why = refused(
            &store,
            &config,
            root(RootCeilingsUpdate {
                readers: Some(1.into()),
                ..RootCeilingsUpdate::default()
            }),
        );
        assert!(why.contains("[jobs] readers"), "{why}");
        let why = refused(
            &store,
            &config,
            root(RootCeilingsUpdate {
                write_mib_per_s: Some(5.into()),
                ..RootCeilingsUpdate::default()
            }),
        );
        assert!(why.contains("[roots.sets] write_mib_per_s"), "{why}");
        for bad in [
            serde_json::json!(0),
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!("4"),
            serde_json::json!(5000),
        ] {
            let why = refused(
                &store,
                &config,
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
            &config,
            root(RootCeilingsUpdate {
                read_mib_per_s: Some(serde_json::json!(0)),
                ..RootCeilingsUpdate::default()
            }),
        );
        assert!(why.contains("the read rate of sets"), "{why}");
        let why = refused(
            &store,
            &config,
            CeilingsUpdate {
                roots: BTreeMap::from([("elsewhere".to_owned(), RootCeilingsUpdate::default())]),
                ..CeilingsUpdate::default()
            },
        );
        assert_eq!(why, "elsewhere is not a root in your configuration.");
        assert_eq!(
            store.settings().unwrap().ceilings,
            CeilingOverrides::default()
        );
    }

    #[test]
    fn your_ceilings_replace_those_you_set_before_and_reach_the_shared_pools() {
        let (_dir, store) = store();
        let config = config("[roots.sets]\npath = \"/sets\"");
        let pools = store.job_pools(&config, "sets").unwrap();
        let set = CeilingsUpdate {
            host: HostCeilingsUpdate {
                hashers: Some(6.into()),
                archive_workers: Some(8.into()),
                concurrent: Some(serde_json::Value::Null),
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
        store
            .update_settings(&config, &update(set), "Operator")
            .unwrap();
        // The default pace: half of each.
        let p = pools.pools();
        assert_eq!((p.readers.limit(), p.hashers.limit()), (5, 3));
        assert_eq!(store.archive_gate(&config).permits(), 4);
        let view = store.settings_view(&config).unwrap().jobs;
        assert_eq!(view.roots[0].write_mib_per_s.in_force, Some(40));

        // A change of pace reaches the same pools.
        let gentle = SettingsUpdate {
            pace: Some(Pace::Gentle),
            ..SettingsUpdate::default()
        };
        store.update_settings(&config, &gentle, "Operator").unwrap();
        assert_eq!((p.readers.limit(), p.hashers.limit()), (2, 1));
        assert_eq!(store.archive_gate(&config).permits(), 2);

        // Setting the Ceilings again replaces them: those left out fall back.
        let only = CeilingsUpdate {
            host: HostCeilingsUpdate {
                hashers: Some(12.into()),
                ..HostCeilingsUpdate::default()
            },
            ..CeilingsUpdate::default()
        };
        store
            .update_settings(&config, &update(only), "Operator")
            .unwrap();
        let yours = store.settings().unwrap().ceilings;
        assert_eq!(yours.host.hashers, Some(12));
        assert_eq!(yours.host.archive_workers, None);
        assert!(yours.roots.is_empty());
        assert_eq!(p.hashers.limit(), 3);
        assert_eq!(
            store.archive_gate(&config).permits(),
            1,
            "a quarter of the default 2, at least 1"
        );
    }
}
