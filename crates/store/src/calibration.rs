//! Calibration Jobs (ADR 0022): measuring how many hash workers the Host sustains and how many payload readers each
//! Root does, and keeping each sweep's curve and knee. A Calibration only reads: it never writes to a Root (ADR 0002).

use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{OptionalExtension, Transaction, params};
use rustix::fs::FileType;
use title_select_api::calibration::{
    Calibration, CalibrationStale, CurvePoint, HostCalibration, Knee, RootCalibration, RootOutcome,
};
use title_select_api::{Job, JobKind, JobState};
use title_select_engine::Progress;
use title_select_engine::calibrate::{self, Limits, RootSweep, Sweep};
use title_select_engine::meter::JobMeter;

use super::jobs::{finish_running, job_by_id};
use super::{Error, Result, Store, now};
use crate::config::Config;
use crate::folder_dir::FolderDir;

/// Why a restart Interrupted a Calibration Job, which is never queued again.
pub(crate) const INTERRUPTED: &str = "the service stopped while the calibration was running; it kept what it had \
     measured. Calibrate again to measure the rest";
/// Why a Calibration you cancelled while it ran was Interrupted.
const CANCELLED: &str = "you cancelled the calibration; it kept what it had measured";
/// Why a Calibration you cancelled before it started was Interrupted.
const CANCELLED_QUEUED: &str = "you cancelled the calibration before it started";

/// How often a running Calibration looks for your cancel between its own checks.
const CANCEL_POLL: Duration = Duration::from_millis(200);

/// The CPUs the Service can use: what the Host's sweep goes up to, and what a Calibration is Stale against.
pub fn cpus() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// How a Calibration runs. Tests shorten its steps.
pub struct Calibrating<'a> {
    /// The most hash workers the Host's sweep tries.
    pub cpus: usize,
    pub limits: Limits,
    /// Called as the Host, then each Root, is measured: `files_done` of `files_total` counts them.
    pub progress: Option<&'a (dyn Fn(Progress) + Sync)>,
}

impl Default for Calibrating<'_> {
    fn default() -> Self {
        Calibrating {
            cpus: cpus(),
            limits: Limits::default(),
            progress: None,
        }
    }
}

/// Ends the Queued Calibration `id` you cancelled.
pub(crate) fn end_queued(tx: &Transaction, id: i64) -> Result<()> {
    tx.execute(
        "UPDATE jobs SET state = 'interrupted', error = ?2, cancel_requested_at = ?3, finished_at = ?3,
                         version = version + 1
         WHERE id = ?1 AND state = 'queued'",
        params![id, CANCELLED_QUEUED, now()],
    )?;
    Ok(())
}

/// Whether a Calibration stopped because you cancelled it.
enum Ran {
    Finished,
    Cancelled,
}

impl Store {
    /// Queues a Calibration, started by `actor`, which runs alone once every Running Job ends. Refused while
    /// another is Queued or Running.
    pub fn enqueue_calibration(&self, actor: &str) -> Result<Job> {
        self.write(|tx| {
            let active: Option<i64> = tx
                .query_row(
                    "SELECT id FROM jobs WHERE kind = 'calibration' AND state IN ('queued', 'running')",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(job) = active {
                return Err(Error::Conflict(format!(
                    "a calibration is already queued or running: job {job}"
                )));
            }
            tx.execute(
                "INSERT INTO jobs (state, kind, actor, readers, hashers, created_at)
                 VALUES ('queued', 'calibration', ?1, ?2, ?3, ?4)",
                params![actor, calibrate::MAX_READERS as i64, cpus() as i64, now()],
            )?;
            job_by_id(tx, tx.last_insert_rowid())
        })
    }

    /// Runs the Running Calibration `id`: the Host's sweep, then each configured Root's in name order, recording
    /// each as it finishes. Once you cancel it, it stops at the next step and ends Interrupted, keeping what it
    /// finished. Returns the Job as it ends.
    pub fn run_calibration(&self, config: &Config, id: i64, how: &Calibrating) -> Result<Job> {
        let _metered = self.meter.job(id).on_this_thread();
        let job = self.job(id)?;
        if job.kind != JobKind::Calibration {
            return Err(Error::Refused(format!("job {id} is not a calibration")));
        }
        if job.state != JobState::Running {
            return Err(Error::Refused(format!(
                "job {id} is {}, not running",
                job.state.as_str()
            )));
        }
        let meter = self.meter.job(id);
        let cancel = AtomicBool::new(false);
        let ran = std::thread::scope(|s| {
            let (done, stop) = std::sync::mpsc::channel::<()>();
            let cancel = &cancel;
            // A step can run for seconds: this ends it soon after you cancel.
            s.spawn(move || {
                while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                    stop.recv_timeout(CANCEL_POLL)
                {
                    if self.cancel_requested(id).unwrap_or(false) {
                        cancel.store(true, Ordering::Relaxed);
                        return;
                    }
                }
            });
            // A sweep that panics fails the Calibration rather than ending the Service's worker without a word.
            let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.calibrate(config, id, how, cancel, &meter)
            }))
            .unwrap_or_else(|panic| {
                let why = panic
                    .downcast_ref::<&str>()
                    .map(|why| why.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "it panicked".to_string());
                Err(Error::Io(format!(
                    "the calibration stopped unexpectedly: {why}"
                )))
            });
            drop(done);
            ran
        });
        let ended = match &ran {
            Ok(Ran::Finished) => self.write(|tx| finish_running(tx, id, JobState::Succeeded, None)),
            Ok(Ran::Cancelled) => {
                self.write(|tx| finish_running(tx, id, JobState::Interrupted, Some(CANCELLED)))
            }
            Err(err) => {
                let message = err.to_string();
                self.write(|tx| finish_running(tx, id, JobState::Failed, Some(&message)))
            }
        };
        ended?;
        // What it measured applies to Jobs from their workers' next file, unless you set those Ceilings yourself.
        self.refresh_ceilings(config)?;
        let job = self.job(id)?;
        match ran {
            Err(err) => Err(err),
            Ok(_) => Ok(job),
        }
    }

    fn calibrate(
        &self,
        config: &Config,
        id: i64,
        how: &Calibrating,
        cancel: &AtomicBool,
        meter: &JobMeter,
    ) -> Result<Ran> {
        let total = 1 + config.roots.len();
        let progress = |files_done| {
            if let Some(progress) = how.progress {
                progress(Progress {
                    files_done,
                    files_total: total,
                });
            }
        };
        let cancelled = || -> Result<bool> {
            if !cancel.load(Ordering::Relaxed) && self.cancel_requested(id)? {
                cancel.store(true, Ordering::Relaxed);
            }
            Ok(cancel.load(Ordering::Relaxed))
        };
        progress(0);
        let host = calibrate::host(how.cpus, how.limits, &Instant::now(), cancel, Some(meter));
        if host.cancelled || cancelled()? {
            return Ok(Ran::Cancelled);
        }
        self.write(|tx| {
            tx.execute(
                "INSERT INTO calibrations (job_id, cpus, curve, knee, knee_bytes_per_s, measured_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    how.cpus as i64,
                    curve_json(&host.curve)?,
                    host.knee.map(|k| k.workers as i64),
                    host.knee.map(|k| k.bytes_per_s),
                    now()
                ],
            )?;
            Ok(())
        })?;
        progress(1);
        for (done, (name, root)) in config.roots.iter().enumerate() {
            if cancelled()? {
                return Ok(Ran::Cancelled);
            }
            let measured = match self.calibration_files(name, &root.path)? {
                Err(err) => unreadable(err.to_string()),
                Ok(files) => {
                    let tally = meter.tally(Some(name));
                    match calibrate::root(&files, how.limits, &Instant::now(), cancel, &tally) {
                        RootSweep::Unreadable(why) => unreadable(why),
                        RootSweep::TimedOut { read } => Measured {
                            outcome: RootOutcome::TimedOut,
                            available: read,
                            sweep: None,
                            error: None,
                        },
                        RootSweep::TooSmall { read } => Measured {
                            outcome: RootOutcome::TooSmall,
                            available: read,
                            sweep: None,
                            error: None,
                        },
                        RootSweep::Swept { sweep, .. } if sweep.cancelled => {
                            return Ok(Ran::Cancelled);
                        }
                        RootSweep::Swept { sweep, read } => Measured {
                            outcome: RootOutcome::Measured,
                            available: read,
                            sweep: Some(sweep),
                            error: None,
                        },
                    }
                }
            };
            self.write(|tx| {
                let (curve, knee) = match &measured.sweep {
                    Some(sweep) => (sweep.curve.as_slice(), sweep.knee),
                    None => (&[][..], None),
                };
                tx.execute(
                    "INSERT INTO calibration_roots
                         (job_id, root, path, outcome, available_bytes, curve, knee, knee_bytes_per_s, error,
                          measured_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        id,
                        name,
                        root.path.display().to_string(),
                        outcome_str(measured.outcome),
                        i64::try_from(measured.available).unwrap_or(i64::MAX),
                        curve_json(curve)?,
                        knee.map(|k| k.workers as i64),
                        knee.map(|k| k.bytes_per_s),
                        measured.error,
                        now()
                    ],
                )?;
                Ok(())
            })?;
            progress(done + 2);
        }
        Ok(Ran::Finished)
    }

    /// The files a Calibration reads from Root `name`, at `path`, in order: those its Source Sets' latest Audits
    /// saw, or, when none has an Audit, those a walk finds. Each is found as the sweep asks for it and opened beneath
    /// the Root without following a symlink. The error says why the Root itself couldn't be opened.
    #[doc(hidden)]
    pub fn calibration_files<'a>(
        &'a self,
        name: &'a str,
        path: &Path,
    ) -> Result<std::io::Result<RootFiles<'a>>> {
        let audited: bool = self.read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM source_sets s JOIN audits a ON a.source_set_id = s.id
                                WHERE s.root = ?1 AND s.unregistered_at IS NULL AND a.status = 'finished')",
                [name],
                |r| r.get(0),
            )?)
        })?;
        let dir = match FolderDir::open(path, "") {
            Ok(dir) => dir,
            Err(err) => return Ok(Err(err)),
        };
        let found = if audited {
            Found::Audited {
                store: self,
                root: name,
                dir,
                sets: HashMap::new(),
                batch: VecDeque::new(),
                offset: 0,
                done: false,
            }
        } else {
            let names = match listed(&dir) {
                Ok(names) => names,
                Err(err) => return Ok(Err(err)),
            };
            Found::Walk {
                stack: vec![(dir, names)],
            }
        };
        Ok(Ok(RootFiles(Mutex::new(found))))
    }

    /// The next `AUDITED_BATCH` files of Root `root` its Source Sets' latest Audits saw, from `offset`, never read
    /// first, then least recently read: each as its Source Set's path and its own.
    fn audited_batch(&self, root: &str, offset: usize) -> Result<Vec<(String, String)>> {
        self.read(|c| {
            let mut stmt = c.prepare_cached(
                "WITH latest AS (
                     SELECT s.id AS set_id, s.path AS set_path,
                            (SELECT a.id FROM audits a WHERE a.source_set_id = s.id AND a.status = 'finished'
                             ORDER BY a.finish_seq DESC LIMIT 1) AS audit_id
                     FROM source_sets s WHERE s.root = ?1 AND s.unregistered_at IS NULL)
                 SELECT l.set_path, e.path,
                        nullif(max(coalesce(max(e.read_at), ''), coalesce(pr.read_at, '')), '') AS last_read
                 FROM latest l
                 JOIN audit_entries e ON e.audit_id = l.audit_id
                 LEFT JOIN payload_reads pr ON pr.source_set_id = l.set_id AND pr.path = e.path
                 WHERE e.path <> ''
                 GROUP BY l.set_id, e.path
                 ORDER BY last_read IS NOT NULL, last_read, l.set_path, e.path
                 LIMIT ?2 OFFSET ?3",
            )?;
            let rows = stmt
                .query_map(
                    params![root, AUDITED_BATCH as i64, offset as i64],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })
    }

    /// The Calibration results that apply under `config`: the latest measurement of the Host and of each configured
    /// Root, and why they are Stale, when they are, with the Service able to use `cpus` CPUs.
    pub fn calibration(&self, config: &Config, cpus: usize) -> Result<Calibration> {
        self.read(|c| {
            let host = c
                .query_row(
                    &format!("{HOST_QUERY} ORDER BY job_id DESC LIMIT 1"),
                    [],
                    host_from_row,
                )
                .optional()?;
            let mut roots = Vec::new();
            let mut stale = Vec::new();
            for (name, root) in &config.roots {
                let found = c
                    .query_row(
                        &format!("{ROOT_QUERY} WHERE root = ?1 ORDER BY job_id DESC LIMIT 1"),
                        [name],
                        root_from_row,
                    )
                    .optional()?;
                let now = root.path.display().to_string();
                match found {
                    None => stale.push(CalibrationStale::Unmeasured { root: name.clone() }),
                    Some(found) => {
                        if found.path != now {
                            stale.push(CalibrationStale::Moved {
                                root: name.clone(),
                                measured: found.path.clone(),
                                now,
                            });
                        }
                        roots.push(found);
                    }
                }
            }
            if let Some(host) = &host
                && host.cpus != cpus
            {
                stale.insert(
                    0,
                    CalibrationStale::Cpus {
                        measured: host.cpus,
                        now: cpus,
                    },
                );
            }
            let mut calibration = Calibration { host, roots, stale };
            if !calibration.measured() {
                calibration.stale.clear();
            }
            Ok(calibration)
        })
    }

    /// What Calibration Job `id` measured: the Host, unless it was cancelled first, and each Root it finished.
    pub fn job_calibration(&self, id: i64) -> Result<Calibration> {
        self.read(|c| {
            let job = job_by_id(c, id)?;
            if job.kind != JobKind::Calibration {
                return Err(Error::NotFound(format!("job {id} is not a calibration")));
            }
            let host = c
                .query_row(
                    &format!("{HOST_QUERY} WHERE job_id = ?1"),
                    [id],
                    host_from_row,
                )
                .optional()?;
            let mut stmt = c.prepare(&format!("{ROOT_QUERY} WHERE job_id = ?1 ORDER BY root"))?;
            let roots = stmt
                .query_map([id], root_from_row)?
                .collect::<rusqlite::Result<_>>()?;
            Ok(Calibration {
                host,
                roots,
                stale: Vec::new(),
            })
        })
    }
}

/// What a Root's measurement found, to record.
struct Measured {
    outcome: RootOutcome,
    available: u64,
    sweep: Option<Sweep>,
    error: Option<String>,
}

fn unreadable(why: String) -> Measured {
    Measured {
        outcome: RootOutcome::Unreadable,
        available: 0,
        sweep: None,
        error: Some(why),
    }
}

/// How many of the files its Audits saw a Root's sweep looks up at a time.
const AUDITED_BATCH: usize = 1024;

/// The files a Calibration reads from a Root, found as its sweep asks for them: see [`Store::calibration_files`].
pub struct RootFiles<'a>(Mutex<Found<'a>>);

enum Found<'a> {
    /// Those its Source Sets' latest Audits saw, a batch at a time, each Source Set's folder opened once.
    Audited {
        store: &'a Store,
        root: &'a str,
        dir: FolderDir,
        sets: HashMap<String, std::io::Result<FolderDir>>,
        batch: VecDeque<(String, String)>,
        offset: usize,
        done: bool,
    },
    /// Those a walk finds, depth first in name order: each folder open with the names in it still to visit,
    /// reversed. The walk passes over symlinks and the folders Title Select keeps for itself.
    Walk {
        stack: Vec<(FolderDir, Vec<(String, FileType)>)>,
    },
}

impl RootFiles<'_> {
    /// The next file, with its path, for the sweep to read; `None` once there are no more.
    #[doc(hidden)]
    pub fn next_at(&self) -> Option<(PathBuf, std::io::Result<File>)> {
        let mut found = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match &mut *found {
            Found::Audited {
                store,
                root,
                dir,
                sets,
                batch,
                offset,
                done,
            } => {
                if batch.is_empty() && !*done {
                    match store.audited_batch(root, *offset) {
                        Ok(rows) => {
                            *done = rows.len() < AUDITED_BATCH;
                            *offset += rows.len();
                            batch.extend(rows);
                        }
                        Err(err) => {
                            *done = true;
                            return Some((
                                dir.path().to_path_buf(),
                                Err(std::io::Error::other(err.to_string())),
                            ));
                        }
                    }
                }
                let (set_path, entry) = batch.pop_front()?;
                let at = dir.path().join(&set_path).join(&entry);
                if set_path.is_empty() {
                    return Some((at, dir.open_file(&entry)));
                }
                let set = sets
                    .entry(set_path.clone())
                    .or_insert_with(|| dir.sub(&set_path));
                Some(match set {
                    Ok(set) => (at, set.open_file(&entry)),
                    Err(err) => (at, Err(std::io::Error::new(err.kind(), err.to_string()))),
                })
            }
            Found::Walk { stack } => loop {
                let (dir, names) = stack.last_mut()?;
                let Some((name, kind)) = names.pop() else {
                    stack.pop();
                    continue;
                };
                if title_select_engine::is_own_entry(&name) {
                    continue;
                }
                let at = dir.path().join(&name);
                let kind = match kind {
                    FileType::Unknown => match dir.stat(&name) {
                        Ok(Some(st)) => FolderDir::kind(&st),
                        Ok(None) => continue,
                        Err(err) => return Some((at, Err(err))),
                    },
                    kind => kind,
                };
                match kind {
                    FileType::RegularFile => return Some((at, dir.open_file(&name))),
                    FileType::Directory => {
                        match dir.sub(&name).and_then(|sub| Ok((listed(&sub)?, sub))) {
                            Ok((names, sub)) => stack.push((sub, names)),
                            Err(err) => return Some((at, Err(err))),
                        }
                    }
                    _ => {}
                }
            },
        }
    }
}

impl calibrate::Files for RootFiles<'_> {
    type Payload = File;

    fn next(&self) -> Option<std::io::Result<File>> {
        self.next_at().map(|(_, file)| file)
    }
}

/// The names in `dir`, in reverse name order to pop from the end.
fn listed(dir: &FolderDir) -> std::io::Result<Vec<(String, FileType)>> {
    let mut names = dir.entries()?;
    names.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(names)
}

fn curve_json(curve: &[CurvePoint]) -> Result<String> {
    serde_json::to_string(curve).map_err(|e| Error::Io(format!("calibration curve: {e}")))
}

fn curve_from(text: &str, column: usize) -> rusqlite::Result<Vec<CurvePoint>> {
    serde_json::from_str(text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, e.into())
    })
}

fn knee_from(workers: Option<i64>, bytes_per_s: Option<f64>) -> Option<Knee> {
    Some(Knee {
        workers: workers? as usize,
        bytes_per_s: bytes_per_s?,
    })
}

fn outcome_str(outcome: RootOutcome) -> &'static str {
    match outcome {
        RootOutcome::Measured => "measured",
        RootOutcome::TooSmall => "too_small",
        RootOutcome::Unreadable => "unreadable",
        RootOutcome::TimedOut => "timed_out",
    }
}

const HOST_QUERY: &str =
    "SELECT job_id, measured_at, cpus, curve, knee, knee_bytes_per_s FROM calibrations";

fn host_from_row(row: &rusqlite::Row) -> rusqlite::Result<HostCalibration> {
    Ok(HostCalibration {
        job: row.get(0)?,
        measured_at: row.get(1)?,
        cpus: row.get::<_, i64>(2)? as usize,
        curve: curve_from(&row.get::<_, String>(3)?, 3)?,
        knee: knee_from(row.get(4)?, row.get(5)?),
    })
}

const ROOT_QUERY: &str =
    "SELECT root, path, job_id, measured_at, outcome, available_bytes, curve, knee,
                                 knee_bytes_per_s, error
                          FROM calibration_roots";

fn root_from_row(row: &rusqlite::Row) -> rusqlite::Result<RootCalibration> {
    let outcome: String = row.get(4)?;
    Ok(RootCalibration {
        root: row.get(0)?,
        path: row.get(1)?,
        job: row.get(2)?,
        measured_at: row.get(3)?,
        outcome: match outcome.as_str() {
            "measured" => RootOutcome::Measured,
            "too_small" => RootOutcome::TooSmall,
            "unreadable" => RootOutcome::Unreadable,
            "timed_out" => RootOutcome::TimedOut,
            other => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    4,
                    rusqlite::types::Type::Text,
                    format!("unknown calibration outcome {other}").into(),
                ));
            }
        },
        available_bytes: row.get::<_, i64>(5)? as u64,
        curve: curve_from(&row.get::<_, String>(6)?, 6)?,
        knee: knee_from(row.get(7)?, row.get(8)?),
        error: row.get(9)?,
    })
}
