//! Calibration Jobs (ADR 0022): measuring how many hash workers the Host sustains and how many payload readers each
//! Root does, and keeping each sweep's curve and knee. A Calibration only reads: it never writes to a Root (ADR 0002).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rusqlite::{OptionalExtension, Transaction, params};
use title_select_api::calibration::{
    Calibration, CalibrationStale, CurvePoint, HostCalibration, Knee, RootCalibration, RootOutcome,
};
use title_select_api::{Job, JobKind, JobState};
use title_select_engine::Progress;
use title_select_engine::calibrate::{self, Candidate, Limits, RootSweep, Sweep};
use title_select_engine::meter::JobMeter;

use super::jobs::{finish_running, job_by_id};
use super::{Error, Result, Store, now};
use crate::config::Config;

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
            let ran = self.calibrate(config, id, how, cancel, &meter);
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
        let enough = how
            .limits
            .sweep_bytes(calibrate::counts(calibrate::MAX_READERS).len());
        for (done, (name, root)) in config.roots.iter().enumerate() {
            if cancelled()? {
                return Ok(Ran::Cancelled);
            }
            let measured = match self.calibration_files(name, &root.path, enough)? {
                Err(err) => Measured {
                    outcome: RootOutcome::Unreadable,
                    available: 0,
                    sweep: None,
                    error: Some(format!("{}: {err}", root.path.display())),
                },
                Ok(files) => {
                    let available = files.iter().map(|f| f.size).sum();
                    let tally = meter.tally(Some(name));
                    match calibrate::root(&files, how.limits, &Instant::now(), cancel, &tally) {
                        RootSweep::TooSmall => Measured {
                            outcome: RootOutcome::TooSmall,
                            available,
                            sweep: None,
                            error: None,
                        },
                        RootSweep::Swept(sweep) if sweep.cancelled => return Ok(Ran::Cancelled),
                        RootSweep::Swept(sweep) => Measured {
                            outcome: RootOutcome::Measured,
                            available,
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

    /// The files a Calibration reads from Root `name`, at `path`, in order, until they add up to `enough`: those its
    /// Source Sets' latest Audits saw, or, when none has an Audit, those a walk finds. The inner error says why the
    /// Root couldn't be walked.
    #[doc(hidden)]
    pub fn calibration_files(
        &self,
        name: &str,
        path: &Path,
        enough: u64,
    ) -> Result<std::io::Result<Vec<Candidate>>> {
        Ok(match self.audited_files(name, path, enough)? {
            Some(files) => Ok(files),
            None => calibrate::walk(path, enough),
        })
    }

    /// The files of Root `name`, at `path`, its Source Sets' latest Audits saw, least recently read first and those
    /// never read before them, until they add up to `enough`; none when no Source Set in it has an Audit.
    fn audited_files(
        &self,
        name: &str,
        path: &Path,
        enough: u64,
    ) -> Result<Option<Vec<Candidate>>> {
        self.read(|c| {
            let audited: bool = c.query_row(
                "SELECT EXISTS (SELECT 1 FROM source_sets s JOIN audits a ON a.source_set_id = s.id
                                WHERE s.root = ?1 AND s.unregistered_at IS NULL AND a.status = 'finished')",
                [name],
                |r| r.get(0),
            )?;
            if !audited {
                return Ok(None);
            }
            let mut stmt = c.prepare(
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
                 ORDER BY last_read IS NOT NULL, last_read, l.set_path, e.path",
            )?;
            let paths = stmt
                .query_map([name], |r| {
                    Ok(under(path, &[r.get::<_, String>(0)?, r.get::<_, String>(1)?]))
                })?
                .filter_map(rusqlite::Result::ok);
            Ok(Some(calibrate::candidates(paths, enough)))
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

/// `parts`, each relative with `/` separators, under `root`.
fn under(root: &Path, parts: &[String]) -> PathBuf {
    parts
        .iter()
        .flat_map(|p| p.split('/'))
        .filter(|p| !p.is_empty())
        .fold(root.to_path_buf(), |path, part| path.join(part))
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
