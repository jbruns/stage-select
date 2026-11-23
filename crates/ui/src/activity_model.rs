//! The Activity screen's pure logic: the counters over its Jobs and where each Job links. Its kept files of every
//! folder are `kept_model`'s.

use stage_select_api::{Job, JobLoad, JobState, JobThroughput, Rates};

use crate::jobs::{JobBook, LiveJob};
use crate::shell::with_failures;

/// How many of the listed Jobs are Running, Queued and Failed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub running: usize,
    pub queued: usize,
    pub failed: usize,
}

pub fn counts<'a>(jobs: impl IntoIterator<Item = &'a Job>) -> Counts {
    let mut counts = Counts::default();
    for job in jobs {
        match job.state {
            JobState::Running => counts.running += 1,
            JobState::Queued => counts.queued += 1,
            JobState::Failed => counts.failed += 1,
            JobState::Succeeded | JobState::Interrupted => {}
        }
    }
    counts
}

pub use crate::parts::outcome::{Outcome, Subject};

/// The Jobs Activity lists: the last good fetch of `/jobs` and of `/failed-jobs` (either absent until one
/// succeeds), merged, with the live book's Jobs over them, so that a failed fetch never hides what streams in.
pub fn listed(recent: Option<&[Job]>, failed: Option<&[Job]>, book: &JobBook) -> Vec<LiveJob> {
    let fetched = with_failures(
        recent.map(<[Job]>::to_vec).unwrap_or_default(),
        failed.map(<[Job]>::to_vec).unwrap_or_default(),
    );
    book.overlay(&fetched)
}

/// The Failed Job the open row (`?row=<job id>`) shows, if it shows one: opening it marks it seen.
pub fn opened_failure<'a>(
    jobs: impl IntoIterator<Item = &'a Job>,
    row: Option<&str>,
) -> Option<i64> {
    let id: i64 = row?.parse().ok()?;
    jobs.into_iter()
        .find(|j| j.id == id && j.state == JobState::Failed)
        .map(|j| j.id)
}

/// A rate of bytes per second in binary units, terse enough for a cell: "312 MiB/s", "4.2 GiB/s", "0 B/s".
pub fn bytes_rate(per_s: f64) -> String {
    const UNITS: [&str; 5] = ["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s"];
    let mut value = per_s.max(0.0);
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match (unit, value < 10.0) {
        (0, _) | (_, false) => format!("{value:.0} {}", UNITS[unit]),
        _ => format!("{value:.1} {}", UNITS[unit]),
    }
}

/// A size in binary units, terse enough for a cell: "312 MiB", "4.2 GiB", "0 B".
pub fn bytes(n: u64) -> String {
    bytes_rate(n as f64).trim_end_matches("/s").to_string()
}

/// A rate against the byte-rate cap in force, if any: "312 MiB/s of 400 MiB/s cap", or "312 MiB/s" uncapped.
pub fn capped_rate(per_s: f64, cap: Option<u64>) -> String {
    match cap {
        Some(cap) => format!("{} of {} cap", bytes_rate(per_s), bytes_rate(cap as f64)),
        None => bytes_rate(per_s),
    }
}

/// Files read and written per second: "12 files/s", "0.5 files/s".
pub fn files_rate(per_s: f64) -> String {
    let per_s = per_s.max(0.0);
    match per_s {
        p if p == 0.0 || p >= 10.0 => format!("{p:.0} files/s"),
        p => format!("{p:.1} files/s"),
    }
}

/// What a Job's row shows of its throughput: while it runs, the bytes it reads and writes per second now, from the
/// latest `rates`; once it has ended, its average over the whole Job, if it was metered.
pub fn throughput_cell(job: &Job, rates: Option<Rates>) -> Option<String> {
    match job.state {
        JobState::Running => rates.map(|r| bytes_rate(r.read_bytes_per_s + r.write_bytes_per_s)),
        JobState::Queued => None,
        _ => job.throughput.and_then(|t| t.bytes_per_s()).map(bytes_rate),
    }
}

/// The payload readers and hash workers at work against what the pace allows: "2 of 4 payload readers, 3 of 8 hash
/// workers".
pub fn workers_fact(load: &JobLoad) -> String {
    format!(
        "{} of {} payload readers, {} of {} hash workers",
        load.readers, load.allowance.readers, load.hashers, load.allowance.hashers
    )
}

/// The Service's CPU use, as a share of the CPUs it may use: "37%".
pub fn cpu_label(share: f64) -> String {
    format!("{:.0}%", (share * 100.0).max(0.0))
}

/// What a finished Job read and wrote, and its average throughput: "1.2 GiB read, 300 MiB written, 41 MiB/s on
/// average".
pub fn average_fact(t: &JobThroughput) -> String {
    let average = t
        .bytes_per_s()
        .map(|r| format!(", {} on average", bytes_rate(r)))
        .unwrap_or_default();
    format!(
        "{} read, {} written{average}",
        bytes(t.bytes_read),
        bytes(t.bytes_written)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use stage_select_api::Workers;

    fn job(id: i64, state: JobState) -> Job {
        Job {
            id,
            kind: Default::default(),
            state,
            source_set: "Atari 2600".into(),
            dat_revision: "ab".repeat(32),
            actor: "operator".into(),
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
            created_at: "2026-01-01T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            version: 1,
        }
    }

    #[test]
    fn listed_jobs_overlay_the_live_book_on_whatever_history_last_loaded() {
        let mut book = JobBook::default();
        let mut streamed = job(5, JobState::Running);
        streamed.version = 2;
        book.replace(vec![streamed]);
        let ids = |jobs: Vec<LiveJob>| {
            jobs.iter()
                .map(|l| (l.job.id, l.job.state))
                .collect::<Vec<_>>()
        };

        assert_eq!(ids(listed(None, None, &book)), [(5, JobState::Running)]);

        let recent = [job(5, JobState::Queued), job(1, JobState::Succeeded)];
        let failed = [job(2, JobState::Failed)];
        assert_eq!(
            ids(listed(Some(&recent), Some(&failed), &book)),
            [
                (5, JobState::Running),
                (2, JobState::Failed),
                (1, JobState::Succeeded)
            ]
        );
        assert_eq!(
            ids(listed(None, Some(&failed), &book)),
            [(5, JobState::Running), (2, JobState::Failed)]
        );
    }

    #[test]
    fn counters_count_running_queued_and_failed_jobs() {
        let jobs = [
            job(1, JobState::Running),
            job(2, JobState::Queued),
            job(3, JobState::Queued),
            job(4, JobState::Failed),
            job(5, JobState::Succeeded),
            job(6, JobState::Interrupted),
        ];
        assert_eq!(
            counts(&jobs),
            Counts {
                running: 1,
                queued: 2,
                failed: 1
            }
        );
    }

    #[test]
    fn a_job_links_to_the_folder_it_works_on_before_its_source_set() {
        let mut scan = job(1, JobState::Running);
        scan.ingest_folder = Some("My Drop".into());
        assert_eq!(Subject::of(&scan), Subject::IngestFolder("My Drop".into()));
        assert_eq!(Subject::of(&scan).href(), "/ingest/My%20Drop");

        let mut build = job(2, JobState::Queued);
        build.library_folder = Some(7);
        assert_eq!(Subject::of(&build).href(), "/libraries/7");

        let mut additions = job(3, JobState::Queued);
        additions.additions_set = Some("No-Intro/Atari - 2600".into());
        assert_eq!(Subject::of(&additions).href(), "/additions");

        let audit = job(4, JobState::Succeeded);
        assert_eq!(Subject::of(&audit).href(), "/source-sets/Atari%202600");
    }

    #[test]
    fn a_finished_job_links_to_what_it_recorded() {
        let mut audit = job(1, JobState::Succeeded);
        audit.audit = Some(12);
        assert_eq!(Outcome::of(&audit), Some(Outcome::Audit(12)));
        assert_eq!(Outcome::of(&audit).unwrap().href(), "/audits/12");

        let mut selection = job(2, JobState::Succeeded);
        selection.selection = Some(5);
        assert_eq!(Outcome::of(&selection).unwrap().href(), "/selections/5");

        let mut build = job(5, JobState::Failed);
        build.build = Some(4);
        assert_eq!(Outcome::of(&build).unwrap().href(), "/builds/4");

        let mut repair = job(3, JobState::Running);
        repair.repair = Some(9);
        assert_eq!(
            Outcome::of(&repair).unwrap().href(),
            "/source-sets/Atari%202600?tab=repair&plan=9"
        );

        assert_eq!(Outcome::of(&job(4, JobState::Queued)), None);
    }

    #[test]
    fn opening_a_failed_job_names_it_and_opening_any_other_does_not() {
        let jobs = [job(3, JobState::Failed), job(4, JobState::Succeeded)];
        assert_eq!(opened_failure(&jobs, Some("3")), Some(3));
        assert_eq!(opened_failure(&jobs, Some("4")), None);
        assert_eq!(opened_failure(&jobs, Some("8")), None);
        assert_eq!(opened_failure(&jobs, Some("x")), None);
        assert_eq!(opened_failure(&jobs, None), None);
    }

    #[test]
    fn rates_read_in_binary_units_terse_enough_for_a_cell() {
        assert_eq!(bytes_rate(0.0), "0 B/s");
        assert_eq!(bytes_rate(900.0), "900 B/s");
        assert_eq!(bytes_rate(4.2 * 1024.0 * 1024.0), "4.2 MiB/s");
        assert_eq!(bytes_rate(312.4 * 1024.0 * 1024.0), "312 MiB/s");
        assert_eq!(bytes_rate(1.5 * 1024.0 * 1024.0 * 1024.0), "1.5 GiB/s");
        assert_eq!(files_rate(0.0), "0 files/s");
        assert_eq!(files_rate(0.5), "0.5 files/s");
        assert_eq!(files_rate(12.3), "12 files/s");
        assert_eq!(cpu_label(0.374), "37%");
    }

    #[test]
    fn a_running_job_shows_its_rate_now_and_a_finished_one_its_average() {
        let mib = 1024.0 * 1024.0;
        let rates = Rates {
            read_bytes_per_s: 300.0 * mib,
            write_bytes_per_s: 12.0 * mib,
            files_per_s: 4.0,
        };
        let running = job(1, JobState::Running);
        assert_eq!(
            throughput_cell(&running, Some(rates)).as_deref(),
            Some("312 MiB/s")
        );
        assert_eq!(
            throughput_cell(&running, None),
            None,
            "no rate before the first sample"
        );
        assert_eq!(
            throughput_cell(&job(2, JobState::Queued), Some(rates)),
            None
        );
        let mut done = job(3, JobState::Succeeded);
        assert_eq!(throughput_cell(&done, None), None, "not metered");
        let t = JobThroughput {
            bytes_read: 3 << 30,
            bytes_written: 1 << 30,
            duration_ms: 100_000,
        };
        done.throughput = Some(t);
        assert_eq!(
            throughput_cell(&done, Some(rates)).as_deref(),
            Some("41 MiB/s")
        );
        assert_eq!(
            average_fact(&t),
            "3.0 GiB read, 1.0 GiB written, 41 MiB/s on average"
        );
    }

    #[test]
    fn a_rate_reads_against_the_cap_in_force_and_alone_when_nothing_caps_it() {
        let mib = 1u64 << 20;
        assert_eq!(
            capped_rate(312.0 * mib as f64, Some(400 * mib)),
            "312 MiB/s of 400 MiB/s cap"
        );
        assert_eq!(capped_rate(312.0 * mib as f64, None), "312 MiB/s");
    }

    #[test]
    fn workers_read_against_what_the_pace_allows() {
        let load = JobLoad {
            job: 1,
            rates: Rates::default(),
            caps: stage_select_api::ByteCaps::default(),
            readers: 2,
            hashers: 3,
            allowance: Workers {
                readers: 4,
                hashers: 8,
            },
            chdman: false,
        };
        assert_eq!(
            workers_fact(&load),
            "2 of 4 payload readers, 3 of 8 hash workers"
        );
    }
}
