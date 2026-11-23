//! The calibration results on the Operations page: one row for the Host and one for each configured Root, and where
//! a Calibration Job that hasn't ended stands.

use stage_select_api::calibration::{Calibration, CalibrationStale, CurvePoint, Knee, RootOutcome};
use stage_select_api::{Job, JobKind, JobProgress, JobState};

use crate::parts::status::CalibrationStatus;

const MIB: f64 = (1u64 << 20) as f64;

/// One row of the results.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// "Host", or the Root's name.
    pub what: String,
    /// What was measured: the CPU count, or the Root's path when it was measured.
    pub of: String,
    pub status: CalibrationStatus,
    pub knee: Option<Knee>,
    pub curve: Vec<CurvePoint>,
    /// Why it is Stale, unreadable or too small, for the reasons under the table.
    pub reason: Option<String>,
}

/// The Host's row, then each configured Root's by name, measured or not.
pub fn rows(calibration: &Calibration) -> Vec<Row> {
    let mut rows = Vec::new();
    let cpus_now = calibration.stale.iter().find_map(|s| match s {
        CalibrationStale::Cpus { now, .. } => Some(*now),
        _ => None,
    });
    rows.push(match &calibration.host {
        Some(host) => Row {
            what: "Host".into(),
            of: format!("{} CPUs", host.cpus),
            status: if cpus_now.is_some() {
                CalibrationStatus::Stale
            } else {
                CalibrationStatus::Measured
            },
            knee: host.knee,
            curve: host.curve.clone(),
            reason: cpus_now.map(|now| {
                format!(
                    "The host was measured with {} CPUs; the service now has {now}.",
                    host.cpus
                )
            }),
        },
        None => Row {
            what: "Host".into(),
            of: String::new(),
            status: CalibrationStatus::NotMeasured,
            knee: None,
            curve: Vec::new(),
            reason: None,
        },
    });
    for root in &calibration.roots {
        let moved = calibration.stale.iter().find_map(|s| match s {
            CalibrationStale::Moved {
                root: name, now, ..
            } if *name == root.root => Some(now.clone()),
            _ => None,
        });
        let (status, reason) = match (root.outcome, &moved) {
            (RootOutcome::Unreadable, _) => (
                CalibrationStatus::Unreadable,
                Some(format!(
                    "Root {} couldn't be read: {}.",
                    root.root,
                    root.error.as_deref().unwrap_or("no reason given")
                )),
            ),
            (_, Some(now)) => (
                CalibrationStatus::Stale,
                Some(format!(
                    "Root {} was measured at {}; it's now at {now}.",
                    root.root, root.path
                )),
            ),
            (RootOutcome::TooSmall, None) => (
                CalibrationStatus::TooSmall,
                Some(format!(
                    "Root {} had {} to read; a measurement needs more.",
                    root.root,
                    size(root.available_bytes)
                )),
            ),
            (RootOutcome::TimedOut, None) => (
                CalibrationStatus::TimedOut,
                Some(format!(
                    "The calibration couldn't find enough to read in root {} in time: it may hold files with \
                     nothing in them, or be slow to list.",
                    root.root
                )),
            ),
            (RootOutcome::Measured, None) => (CalibrationStatus::Measured, None),
        };
        rows.push(Row {
            what: root.root.clone(),
            of: root.path.clone(),
            status,
            knee: root.knee,
            curve: root.curve.clone(),
            reason,
        });
    }
    for stale in &calibration.stale {
        if let CalibrationStale::Unmeasured { root } = stale {
            rows.push(Row {
                what: root.clone(),
                of: String::new(),
                status: CalibrationStatus::NotMeasured,
                knee: None,
                curve: Vec::new(),
                reason: None,
            });
        }
    }
    rows[1..].sort_by(|a, b| a.what.cmp(&b.what));
    rows
}

fn size(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / MIB)
}

/// A throughput in MiB/s, to one decimal place.
pub fn mib_per_s(bytes_per_s: f64) -> String {
    format!("{:.1}", bytes_per_s / MIB)
}

/// A sweep's curve: each worker count and its throughput in MiB/s.
pub fn curve(points: &[CurvePoint]) -> String {
    points
        .iter()
        .map(|p| format!("{}: {}", p.workers, mib_per_s(p.bytes_per_s)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The Calibration Job that hasn't ended, if one hasn't, and where it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    pub job: i64,
    pub cancelling: bool,
    pub stands: String,
}

/// Where the Calibration that hasn't ended stands among `jobs`: waiting for the running ones, or measuring.
pub fn active<'a>(
    jobs: impl IntoIterator<Item = (&'a Job, Option<&'a JobProgress>)>,
) -> Option<Active> {
    let jobs: Vec<_> = jobs.into_iter().collect();
    let (job, progress) = jobs
        .iter()
        .find(|(j, _)| j.kind == JobKind::Calibration && !j.state.is_final())?;
    let stands = match job.state {
        JobState::Queued => {
            let running = jobs
                .iter()
                .filter(|(j, _)| j.state == JobState::Running)
                .count();
            match running {
                0 => "Starting".to_owned(),
                1 => "Waiting for 1 running job".to_owned(),
                n => format!("Waiting for {n} running jobs"),
            }
        }
        _ => match progress {
            Some(p) if p.files_total > 0 => {
                format!("Measuring: {} of {} done", p.files_done, p.files_total)
            }
            _ => "Measuring".to_owned(),
        },
    };
    Some(Active {
        job: job.id,
        cancelling: job.cancel_requested_at.is_some(),
        stands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use stage_select_api::Workers;
    use stage_select_api::calibration::{HostCalibration, RootCalibration};

    fn job(id: i64, kind: JobKind, state: JobState) -> Job {
        Job {
            id,
            kind,
            state,
            source_set: String::new(),
            dat_revision: String::new(),
            actor: "Operator token".into(),
            attempt: 1,
            requeued_from: None,
            audit: None,
            profile: None,
            selection: None,
            library_folder: None,
            ingest_folder: None,
            repair: None,
            additions_set: None,
            cancel_requested_at: None,
            plan: None,
            evidence: None,
            error: None,
            romm_scan: None,
            build: None,
            throughput: None,
            workers: Workers {
                readers: 1,
                hashers: 1,
            },
            created_at: "now".into(),
            started_at: None,
            finished_at: None,
            version: 1,
        }
    }

    fn root(name: &str, outcome: RootOutcome) -> RootCalibration {
        RootCalibration {
            root: name.into(),
            path: format!("/srv/{name}"),
            job: 1,
            measured_at: "now".into(),
            outcome,
            available_bytes: 1 << 20,
            curve: Vec::new(),
            knee: None,
            error: None,
        }
    }

    #[test]
    fn nothing_measured_is_a_host_row_not_measured() {
        let rows = rows(&Calibration::default());

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, CalibrationStatus::NotMeasured);
    }

    #[test]
    fn each_row_takes_its_status_and_stale_reason() {
        let calibration = Calibration {
            host: Some(HostCalibration {
                job: 1,
                measured_at: "now".into(),
                cpus: 4,
                curve: Vec::new(),
                knee: None,
            }),
            roots: vec![
                root("moved", RootOutcome::Measured),
                root("small", RootOutcome::TooSmall),
                root("slow", RootOutcome::TimedOut),
                root("fine", RootOutcome::Measured),
                RootCalibration {
                    error: Some("permission denied".into()),
                    ..root("broken", RootOutcome::Unreadable)
                },
            ],
            stale: vec![
                CalibrationStale::Cpus {
                    measured: 4,
                    now: 8,
                },
                CalibrationStale::Unmeasured { root: "new".into() },
                CalibrationStale::Moved {
                    root: "moved".into(),
                    measured: "/srv/moved".into(),
                    now: "/mnt/moved".into(),
                },
            ],
        };

        let rows = rows(&calibration);

        let got: Vec<(&str, CalibrationStatus)> =
            rows.iter().map(|r| (r.what.as_str(), r.status)).collect();
        assert_eq!(
            got,
            [
                ("Host", CalibrationStatus::Stale),
                ("broken", CalibrationStatus::Unreadable),
                ("fine", CalibrationStatus::Measured),
                ("moved", CalibrationStatus::Stale),
                ("new", CalibrationStatus::NotMeasured),
                ("slow", CalibrationStatus::TimedOut),
                ("small", CalibrationStatus::TooSmall),
            ]
        );
        assert!(rows[0].reason.as_deref().unwrap().contains("4 CPUs"));
        assert!(
            rows[1]
                .reason
                .as_deref()
                .unwrap()
                .contains("permission denied")
        );
        assert!(rows[3].reason.as_deref().unwrap().contains("/mnt/moved"));
        assert!(rows[5].reason.as_deref().unwrap().contains("in time"));
        assert!(rows[6].reason.as_deref().unwrap().contains("1.0 MiB"));
    }

    #[test]
    fn a_queued_calibration_waits_for_the_running_jobs() {
        let calibration = job(3, JobKind::Calibration, JobState::Queued);
        let running = [
            job(1, JobKind::Audit, JobState::Running),
            job(2, JobKind::Build, JobState::Running),
        ];
        let queued = job(4, JobKind::Audit, JobState::Queued);

        let waiting = active(
            running
                .iter()
                .chain([&calibration, &queued])
                .map(|j| (j, None)),
        )
        .unwrap();

        assert_eq!(waiting.job, 3);
        assert_eq!(waiting.stands, "Waiting for 2 running jobs");
    }

    #[test]
    fn a_running_calibration_counts_what_it_measured() {
        let calibration = job(3, JobKind::Calibration, JobState::Running);
        let progress = JobProgress {
            job: 3,
            version: 1,
            files_done: 1,
            files_total: 3,
            rates: None,
        };

        let measuring = active([(&calibration, Some(&progress))]).unwrap();

        assert_eq!(measuring.stands, "Measuring: 1 of 3 done");
        assert_eq!(
            active([(&job(5, JobKind::Calibration, JobState::Succeeded), None)]),
            None
        );
    }
}
