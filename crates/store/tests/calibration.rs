//! Calibration Jobs: how one queues ahead of every other Job and runs alone, what it reads and records, and when
//! its results are Stale, from synthetic Roots with tiny step limits.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::time::Duration;

use common::*;
use title_select_api::calibration::{CalibrationStale, RootOutcome};
use title_select_api::{AuditEvidence, JobKind, JobState, Workers};
use title_select_engine::calibrate::Limits;
use title_select_store::calibration::Calibrating;
use title_select_store::config::Config;
use title_select_store::{DATABASE, Error, Store};

const KIB: usize = 1 << 10;

/// Steps short enough for a test: each reads at most 64 KiB, and a Root needs 64 KiB to be measured.
fn limits() -> Limits {
    Limits {
        step_time: Duration::from_secs(30),
        step_bytes: 64 * KIB as u64,
        min_bytes: 64 * KIB as u64,
        ..Limits::default()
    }
}

fn how(cpus: usize) -> Calibrating<'static> {
    Calibrating {
        cpus,
        limits: limits(),
        progress: None,
    }
}

/// A store with the Source Sets Alpha and Beta, audited, in the read-only Root `sets`; the Root `plain`, with no
/// Audits and a megabyte of files; and the Ingest Folder `in`.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in ["sets/Alpha", "sets/Beta", "dats", "plain", "in"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        for i in 0..64 {
            write_file(
                &tmp.path().join(format!("plain/{}/{i:02}.bin", i % 4)),
                &vec![i as u8; 16 * KIB],
            );
        }
        write_file(&tmp.path().join("in/new.zip"), b"new");
        let config = Env::config(tmp.path(), &[("sets", "sets"), ("plain", "plain")]);
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let env = Env { tmp, store, config };
        let dat = write_dat_version(
            &env.tmp.path().join("dats"),
            "test.dat",
            "1",
            &[
                ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
                ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
            ],
        );
        env.store.import_dat(&dat, None).unwrap();
        for set in ["Alpha", "Beta"] {
            env.store
                .add_source_set(&env.config, "sets", set, "No-Intro/Test - System")
                .unwrap();
            write_file(&env.set(set).join("Pong (USA).a26"), b"pong");
            write_zip(
                &env.set(set).join("Combat (USA).zip"),
                &[("Combat (USA).a26", b"combat")],
            );
            env.store
                .audit_source_set(&env.config, set, None, AuditEvidence::Full)
                .unwrap();
        }
        env
    }

    /// A configuration with each `(name, folder)` Root, and the Ingest Folder `in`.
    fn config(base: &Path, roots: &[(&str, &str)]) -> Config {
        let mut text = String::new();
        for (name, folder) in roots {
            text += &format!(
                "[roots.{name}]\npath = {:?}\n",
                base.join(folder).display().to_string()
            );
        }
        text += &format!(
            "[roots.in]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n",
            base.join("in").display().to_string()
        );
        Config::parse(&text).unwrap()
    }

    fn set(&self, name: &str) -> PathBuf {
        self.tmp.path().join("sets").join(name)
    }

    fn enqueue_audit(&self, set: &str) -> title_select_api::Job {
        self.store
            .enqueue_audit(
                set,
                None,
                "Operator",
                Workers {
                    readers: 1,
                    hashers: 1,
                },
                AuditEvidence::Full,
            )
            .unwrap()
    }

    /// Queues a Calibration, claims it and runs it.
    fn calibrate(&self, how: &Calibrating) -> title_select_api::Job {
        let job = self.store.enqueue_calibration("Operator").unwrap();
        assert_eq!(self.store.claim_job().unwrap().unwrap().id, job.id);
        self.store
            .run_calibration(&self.config, job.id, how)
            .unwrap()
    }

    /// Every entry under the temporary folder but the store, with its size, modification time and content.
    fn snapshot(&self) -> Vec<(String, u64, std::time::SystemTime, Vec<u8>)> {
        walkdir::WalkDir::new(self.tmp.path())
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.file_name() != "store")
            .map(|e| e.unwrap())
            .map(|e| {
                let meta = e.metadata().unwrap();
                let content = if meta.is_file() {
                    std::fs::read(e.path()).unwrap()
                } else {
                    Vec::new()
                };
                (
                    e.path().display().to_string(),
                    meta.len(),
                    meta.modified().unwrap(),
                    content,
                )
            })
            .collect()
    }
}

#[test]
fn a_calibration_measures_the_host_and_every_root_and_writes_nothing() {
    let env = Env::new();
    let before = env.snapshot();
    let job = env.calibrate(&how(3));
    assert_eq!(job.kind, JobKind::Calibration);
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    assert_eq!(env.snapshot(), before, "a calibration writes to no root");

    let measured = env.store.job_calibration(job.id).unwrap();
    let host = measured.host.unwrap();
    assert_eq!(host.cpus, 3);
    assert_eq!(
        host.curve.iter().map(|p| p.workers).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(host.knee.is_some());

    let roots: Vec<_> = measured
        .roots
        .iter()
        .map(|r| (r.root.as_str(), r.outcome))
        .collect();
    assert_eq!(
        roots,
        [
            ("in", RootOutcome::TooSmall),
            ("plain", RootOutcome::Measured),
            ("sets", RootOutcome::TooSmall),
        ],
        "every root, the ingest folder included"
    );
    let plain = &measured.roots[1];
    assert_eq!(
        plain.path,
        env.tmp.path().join("plain").display().to_string()
    );
    assert!(!plain.curve.is_empty());
    assert!(plain.knee.is_some());
    let read: u64 = plain.curve.iter().map(|p| p.bytes).sum();
    assert_eq!(plain.available_bytes, read, "what its steps read");
    assert!(read < 1 << 20, "each file read at most once");
    let small = &measured.roots[2];
    assert!(small.curve.is_empty() && small.knee.is_none());
}

#[test]
fn a_root_that_cannot_be_read_is_recorded_unreadable() {
    let env = Env::new();
    std::fs::remove_dir_all(env.tmp.path().join("plain")).unwrap();
    let job = env.calibrate(&how(1));
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let plain = env
        .store
        .job_calibration(job.id)
        .unwrap()
        .roots
        .into_iter()
        .find(|r| r.root == "plain")
        .unwrap();
    assert_eq!(plain.outcome, RootOutcome::Unreadable);
    assert!(plain.error.is_some());
}

#[test]
fn a_calibration_waits_for_running_jobs_holds_queued_ones_and_is_refused_a_second_time() {
    let env = Env::new();
    let running = env.enqueue_audit("Alpha");
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, running.id);
    let held = env.enqueue_audit("Beta");

    let calibration = env.store.enqueue_calibration("Operator").unwrap();
    assert_eq!(calibration.state, JobState::Queued);
    assert!(matches!(
        env.store.enqueue_calibration("Operator"),
        Err(Error::Conflict(_))
    ));
    assert_eq!(
        env.store.claim_job().unwrap(),
        None,
        "the calibration waits for the running audit, and the queued one waits behind it"
    );

    env.store
        .run_audit_job(&env.config, running.id, None)
        .unwrap();
    let claimed = env.store.claim_job().unwrap().unwrap();
    assert_eq!(
        claimed.id, calibration.id,
        "it goes ahead of the queued audit"
    );
    assert_eq!(
        env.store.claim_job().unwrap(),
        None,
        "nothing runs alongside it"
    );
    assert!(matches!(
        env.store.enqueue_calibration("Operator"),
        Err(Error::Conflict(_))
    ));

    let ended = env
        .store
        .run_calibration(&env.config, calibration.id, &how(1))
        .unwrap();
    assert_eq!(ended.state, JobState::Succeeded);
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, held.id);
    env.store.enqueue_calibration("Operator").unwrap();
}

#[test]
fn a_cancelled_calibration_keeps_the_roots_it_finished() {
    let env = Env::new();
    let job = env.store.enqueue_calibration("Operator").unwrap();
    env.store.claim_job().unwrap().unwrap();
    // Cancelled once the Host and the first Root, `in`, are measured.
    let cancel = |p: title_select_engine::Progress| {
        if p.files_done == 2 {
            env.store.cancel_job(job.id).unwrap();
        }
    };
    let ended = env
        .store
        .run_calibration(
            &env.config,
            job.id,
            &Calibrating {
                progress: Some(&cancel),
                ..how(1)
            },
        )
        .unwrap();
    assert_eq!(ended.state, JobState::Interrupted);
    assert!(ended.error.unwrap().contains("you cancelled"));
    let kept = env.store.job_calibration(job.id).unwrap();
    assert!(kept.host.is_some());
    assert_eq!(
        kept.roots
            .iter()
            .map(|r| r.root.as_str())
            .collect::<Vec<_>>(),
        ["in"]
    );
    let now = env.store.calibration(&env.config, 1).unwrap();
    assert_eq!(
        now.stale,
        [
            CalibrationStale::Unmeasured {
                root: "plain".into()
            },
            CalibrationStale::Unmeasured {
                root: "sets".into()
            },
        ]
    );
}

#[test]
fn a_queued_calibration_you_cancel_ends_at_once_and_holds_nothing_back() {
    let env = Env::new();
    let running = env.enqueue_audit("Alpha");
    env.store.claim_job().unwrap().unwrap();
    let held = env.enqueue_audit("Beta");
    let calibration = env.store.enqueue_calibration("Operator").unwrap();
    let cancelled = env.store.cancel_job(calibration.id).unwrap();
    assert_eq!(cancelled.state, JobState::Interrupted);
    assert!(cancelled.cancel_requested_at.is_some());
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, held.id);
    env.store
        .run_audit_job(&env.config, running.id, None)
        .unwrap();
}

#[test]
fn a_restart_interrupts_a_running_calibration_without_queueing_it_again() {
    let env = Env::new();
    let job = env.store.enqueue_calibration("Operator").unwrap();
    env.store.claim_job().unwrap().unwrap();
    let recovered = env
        .store
        .recover_jobs(Workers {
            readers: 1,
            hashers: 1,
        })
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].job.id, job.id);
    assert_eq!(recovered[0].job.state, JobState::Interrupted);
    assert_eq!(recovered[0].requeued, None);
}

#[test]
fn files_never_read_come_first_then_the_least_recently_read() {
    let env = Env::new();
    {
        let db = rusqlite::Connection::open(env.tmp.path().join("store").join(DATABASE)).unwrap();
        let read_at = |set: &str, path: &str, at: Option<&str>| {
            db.execute(
                "UPDATE audit_entries SET read_at = ?3 WHERE path = ?2 AND audit_id IN
                     (SELECT a.id FROM audits a JOIN source_sets s ON s.id = a.source_set_id WHERE s.name = ?1)",
                rusqlite::params![set, path, at],
            )
            .unwrap();
            db.execute(
                "DELETE FROM payload_reads WHERE path = ?2
                     AND source_set_id = (SELECT id FROM source_sets WHERE name = ?1)",
                rusqlite::params![set, path],
            )
            .unwrap();
        };
        read_at("Alpha", "Pong (USA).a26", Some("2020-01-01T00:00:00Z"));
        read_at("Alpha", "Combat (USA).zip", None);
        read_at("Beta", "Pong (USA).a26", Some("2010-01-01T00:00:00Z"));
        read_at("Beta", "Combat (USA).zip", Some("2030-01-01T00:00:00Z"));
    }
    assert_eq!(
        found(&env, "sets"),
        [
            env.set("Alpha").join("Combat (USA).zip"),
            env.set("Beta").join("Pong (USA).a26"),
            env.set("Alpha").join("Pong (USA).a26"),
            env.set("Beta").join("Combat (USA).zip"),
        ]
    );
}

#[test]
fn a_root_with_no_audits_is_walked() {
    let env = Env::new();
    let files = found(&env, "plain");
    assert_eq!(files.len(), 64);
    assert_eq!(files[0], env.tmp.path().join("plain/0/00.bin"));
    assert_eq!(files[1], env.tmp.path().join("plain/0/04.bin"));
}

/// The files a calibration reads from Root `root`, in order, of those it found; the ones it couldn't open left out.
fn found(env: &Env, root: &str) -> Vec<PathBuf> {
    let files = env
        .store
        .calibration_files(root, &env.tmp.path().join(root))
        .unwrap()
        .unwrap();
    std::iter::from_fn(|| files.next_at())
        .filter_map(|(path, file)| file.is_ok().then_some(path))
        .collect()
}

#[test]
fn a_calibration_never_follows_a_symlink_out_of_a_root() {
    let env = Env::new();
    let outside = env.tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    // An audited Source Set's folder swapped for a symlink to a copy of it outside the Root.
    std::fs::rename(env.set("Beta"), outside.join("Beta")).unwrap();
    std::os::unix::fs::symlink(outside.join("Beta"), env.set("Beta")).unwrap();
    // A walked Root's folder, and a file in it, that are symlinks out of it.
    write_file(&outside.join("secret/payload.bin"), &[7; 16 * KIB]);
    std::os::unix::fs::symlink(outside.join("secret"), env.tmp.path().join("plain/0/link"))
        .unwrap();
    std::os::unix::fs::symlink(
        outside.join("secret/payload.bin"),
        env.tmp.path().join("plain/1/file.bin"),
    )
    .unwrap();

    let sets = found(&env, "sets");
    assert_eq!(sets.len(), 2, "{sets:?}");
    assert!(
        sets.iter().all(|p| p.starts_with(env.set("Alpha"))),
        "{sets:?}"
    );
    let plain = found(&env, "plain");
    assert_eq!(plain.len(), 64, "only the files really in it: {plain:?}");
}

#[test]
fn a_root_none_of_whose_files_can_be_opened_is_recorded_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new();
    let plain = env.tmp.path().join("plain");
    let files: Vec<PathBuf> = walkdir::WalkDir::new(&plain)
        .into_iter()
        .map(|e| e.unwrap().into_path())
        .filter(|p| p.is_file())
        .collect();
    for file in &files {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    if std::fs::File::open(&files[0]).is_ok() {
        // Running as a user that reads past permissions: nothing to test.
        return;
    }
    let job = env.calibrate(&how(1));
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let plain = env
        .store
        .job_calibration(job.id)
        .unwrap()
        .roots
        .into_iter()
        .find(|r| r.root == "plain")
        .unwrap();
    assert_eq!(plain.outcome, RootOutcome::Unreadable, "{plain:?}");
    assert!(plain.curve.is_empty() && plain.knee.is_none());
    assert!(plain.error.is_some_and(|e| e.contains("ermission denied")));
}

#[test]
fn an_audited_root_that_is_gone_is_recorded_unreadable() {
    let env = Env::new();
    std::fs::remove_dir_all(env.tmp.path().join("sets")).unwrap();
    let job = env.calibrate(&how(1));
    let sets = env
        .store
        .job_calibration(job.id)
        .unwrap()
        .roots
        .into_iter()
        .find(|r| r.root == "sets")
        .unwrap();
    assert_eq!(sets.outcome, RootOutcome::Unreadable, "{sets:?}");
    assert!(sets.error.is_some());
}

#[test]
fn calibration_results_are_stale_when_the_cpus_or_the_roots_change() {
    let env = Env::new();
    assert_eq!(
        env.store.calibration(&env.config, 4).unwrap(),
        Default::default(),
        "nothing measured is not stale"
    );
    let job = env.calibrate(&how(4));
    let now = env.store.calibration(&env.config, 4).unwrap();
    assert_eq!(now.stale, []);
    assert_eq!(now.host.as_ref().unwrap().job, job.id);
    assert_eq!(now.roots.len(), 3);

    assert_eq!(
        env.store.calibration(&env.config, 8).unwrap().stale,
        [CalibrationStale::Cpus {
            measured: 4,
            now: 8
        }]
    );

    std::fs::create_dir_all(env.tmp.path().join("more")).unwrap();
    let added = Env::config(
        env.tmp.path(),
        &[("sets", "sets"), ("plain", "plain"), ("more", "more")],
    );
    assert_eq!(
        env.store.calibration(&added, 4).unwrap().stale,
        [CalibrationStale::Unmeasured {
            root: "more".into()
        }]
    );

    let moved = Env::config(env.tmp.path(), &[("sets", "sets"), ("plain", "more")]);
    let found = env.store.calibration(&moved, 4).unwrap();
    assert_eq!(
        found.stale,
        [CalibrationStale::Moved {
            root: "plain".into(),
            measured: env.tmp.path().join("plain").display().to_string(),
            now: env.tmp.path().join("more").display().to_string(),
        }]
    );
    assert_eq!(found.roots.len(), 3, "stale results keep applying");

    let removed = Env::config(env.tmp.path(), &[("sets", "sets")]);
    assert_eq!(
        env.store.calibration(&removed, 4).unwrap().roots.len(),
        2,
        "only configured roots"
    );
}
