//! Prints what a Calibration measured, for `calibrate --server`.

use title_select_api::calibration::{
    Calibration, CalibrationStale, CurvePoint, Knee, RootCalibration, RootOutcome,
};

const MIB: f64 = (1u64 << 20) as f64;

/// A table of the Host's and each Root's knee and curve, one row each.
pub fn table(calibration: &Calibration) -> String {
    let mut rows = vec![[
        "measured".to_owned(),
        "workers at the knee".to_owned(),
        "MiB/s at the knee".to_owned(),
        "curve (workers: MiB/s)".to_owned(),
    ]];
    if let Some(host) = &calibration.host {
        let (workers, rate) = knee(host.knee);
        rows.push([
            format!("host, {} CPUs: hash workers", host.cpus),
            workers,
            rate,
            curve(&host.curve),
        ]);
    }
    for root in &calibration.roots {
        rows.push(root_row(root));
    }
    let widths: Vec<usize> = (0..4)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &rows {
        let line: Vec<String> = row
            .iter()
            .zip(&widths)
            .map(|(cell, width)| format!("{cell:width$}"))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

fn root_row(root: &RootCalibration) -> [String; 4] {
    let what = format!("root {} ({}): payload readers", root.root, root.path);
    match root.outcome {
        RootOutcome::Measured => {
            let (workers, rate) = knee(root.knee);
            [what, workers, rate, curve(&root.curve)]
        }
        RootOutcome::TooSmall => [
            what,
            "-".into(),
            "-".into(),
            format!(
                "too small to measure: {:.1} MiB to read in all",
                root.available_bytes as f64 / MIB
            ),
        ],
        RootOutcome::TimedOut => [
            what,
            "-".into(),
            "-".into(),
            "timed out: it couldn't find enough to read in time".into(),
        ],
        RootOutcome::Unreadable => [
            what,
            "-".into(),
            "-".into(),
            format!(
                "couldn't read it: {}",
                root.error.as_deref().unwrap_or("no reason given")
            ),
        ],
    }
}

fn knee(knee: Option<Knee>) -> (String, String) {
    match knee {
        Some(k) => (k.workers.to_string(), format!("{:.1}", k.bytes_per_s / MIB)),
        None => ("-".into(), "-".into()),
    }
}

fn curve(curve: &[CurvePoint]) -> String {
    curve
        .iter()
        .map(|p| format!("{}: {:.1}", p.workers, p.bytes_per_s / MIB))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why the results that apply now are stale, one line each.
pub fn stale(calibration: &Calibration) -> Vec<String> {
    calibration
        .stale
        .iter()
        .map(|s| match s {
            CalibrationStale::Cpus { measured, now } => format!(
                "the calibration is stale: it measured {measured} CPUs and the service now has {now}"
            ),
            CalibrationStale::Unmeasured { root } => {
                format!("the calibration is stale: root {root} isn't measured yet")
            }
            CalibrationStale::Moved {
                root,
                measured,
                now,
            } => format!(
                "the calibration is stale: root {root} moved from {measured} to {now} since it was measured"
            ),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::calibration::HostCalibration;

    fn point(workers: usize, mib_per_s: f64) -> CurvePoint {
        CurvePoint {
            workers,
            bytes: 0,
            duration_ms: 0,
            bytes_per_s: mib_per_s * MIB,
        }
    }

    fn root(name: &str, outcome: RootOutcome) -> RootCalibration {
        RootCalibration {
            root: name.into(),
            path: format!("/srv/{name}"),
            job: 1,
            measured_at: "now".into(),
            outcome,
            available_bytes: 3 << 20,
            curve: vec![],
            knee: None,
            error: None,
        }
    }

    #[test]
    fn the_table_has_a_row_for_the_host_and_each_root() {
        let calibration = Calibration {
            host: Some(HostCalibration {
                job: 1,
                measured_at: "now".into(),
                cpus: 4,
                curve: vec![point(1, 100.0), point(2, 200.0), point(4, 210.0)],
                knee: Some(Knee {
                    workers: 2,
                    bytes_per_s: 200.0 * MIB,
                }),
            }),
            roots: vec![
                RootCalibration {
                    curve: vec![point(1, 50.0), point(2, 40.0)],
                    knee: Some(Knee {
                        workers: 1,
                        bytes_per_s: 50.0 * MIB,
                    }),
                    ..root("sets", RootOutcome::Measured)
                },
                root("ingest", RootOutcome::TooSmall),
                RootCalibration {
                    error: Some("permission denied".into()),
                    ..root("gone", RootOutcome::Unreadable)
                },
            ],
            stale: vec![],
        };

        let table = table(&calibration);

        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 5, "{table}");
        assert!(lines[0].starts_with("measured"), "{table}");
        assert!(lines[1].contains("host, 4 CPUs"), "{table}");
        assert!(
            lines[1].contains("200.0") && lines[1].contains("1: 100.0, 2: 200.0, 4: 210.0"),
            "{table}"
        );
        assert!(lines[2].contains("root sets (/srv/sets)"), "{table}");
        assert!(lines[2].contains("1: 50.0, 2: 40.0"), "{table}");
        assert!(
            lines[3].contains("too small to measure: 3.0 MiB to read"),
            "{table}"
        );
        assert!(
            lines[4].contains("couldn't read it: permission denied"),
            "{table}"
        );
    }

    #[test]
    fn each_stale_reason_is_a_line() {
        let calibration = Calibration {
            stale: vec![
                CalibrationStale::Cpus {
                    measured: 4,
                    now: 8,
                },
                CalibrationStale::Unmeasured {
                    root: "ingest".into(),
                },
                CalibrationStale::Moved {
                    root: "sets".into(),
                    measured: "/a".into(),
                    now: "/b".into(),
                },
            ],
            ..Calibration::default()
        };

        let lines = stale(&calibration);

        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("4 CPUs and the service now has 8"));
        assert!(lines[1].contains("root ingest isn't measured"));
        assert!(lines[2].contains("moved from /a to /b"));
    }
}
