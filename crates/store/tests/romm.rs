//! The RomM client, its connection test and the scan a Build requests, against a stub RomM on a local port that
//! answers as RomM 5.4.1 does (research 0136). No test sends a request to a real RomM.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use title_select_api::romm::{RommCheck, RommUser};
use title_select_store::romm::{RommClient, RommError};

/// A request the stub saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
    body: String,
}

type Handler = dyn Fn(&Seen) -> (u16, String) + Send + Sync;

/// A stub RomM: an HTTP/1.1 server on a local port that answers each request with `handler` and records it.
struct StubRomm {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl StubRomm {
    fn start(handler: impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static) -> StubRomm {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let handler: Arc<Handler> = Arc::new(handler);
        let s = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let mut words = line.split_whitespace();
                let method = words.next().unwrap_or("").to_owned();
                let path = words.next().unwrap_or("").to_owned();
                let (mut authorization, mut length) = (None, 0usize);
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        let value = value.trim().to_owned();
                        if name.eq_ignore_ascii_case("authorization") {
                            authorization = Some(value);
                        } else if name.eq_ignore_ascii_case("content-length") {
                            length = value.parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0; length];
                let _ = reader.read_exact(&mut body);
                let request = Seen {
                    method,
                    path,
                    authorization,
                    body: String::from_utf8_lossy(&body).into_owned(),
                };
                s.lock().unwrap().push(request.clone());
                let (status, body) = handler(&request);
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        StubRomm { url, seen }
    }

    /// A stub answering as RomM does: a heartbeat for anyone, then each route only to `token`, and only with its
    /// scope among `scopes`. The token's user is `svc`, an admin if `admin`.
    fn like_romm(version: &str, token: &str, scopes: &[&str], admin: bool) -> StubRomm {
        let version = version.to_owned();
        let bearer = format!("Bearer {token}");
        let scopes: BTreeSet<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
        StubRomm::start(move |r| {
            if (r.method.as_str(), r.path.as_str()) == ("GET", "/api/heartbeat") {
                return (
                    200,
                    format!(
                        r#"{{"SYSTEM": {{"VERSION": "{version}", "SHOW_SETUP_WIZARD": false}}}}"#
                    ),
                );
            }
            if r.authorization.as_deref() != Some(bearer.as_str()) {
                return (401, r#"{"detail": "Not authenticated"}"#.into());
            }
            let (scope, answer) = match (r.method.as_str(), r.path.as_str()) {
                ("GET", "/api/users/me") => (
                    "me.read",
                    (
                        200,
                        format!(
                            r#"{{"id": 3, "username": "svc", "role": "{}", "oauth_scopes": []}}"#,
                            if admin { "admin" } else { "editor" }
                        ),
                    ),
                ),
                ("GET", p) if p.starts_with("/api/roms?") => {
                    ("roms.read", (200, r#"{"items": [], "total": 0}"#.into()))
                }
                ("GET", "/api/platforms/identifiers") => ("platforms.read", (200, "[1, 2]".into())),
                ("GET", "/api/collections/identifiers") => ("collections.read", (200, "[]".into())),
                ("POST", "/api/collections/0/roms") => (
                    "collections.write",
                    (
                        404,
                        r#"{"detail": "Collection with id '0' not found"}"#.into(),
                    ),
                ),
                ("GET", p) if p.starts_with("/api/tasks/") && p != "/api/tasks/status" => {
                    ("tasks.run", (404, r#"{"detail": "Task not found"}"#.into()))
                }
                _ => return (404, r#"{"detail": "Not Found"}"#.into()),
            };
            if scopes.contains(scope) {
                answer
            } else {
                (403, r#"{"detail": "Forbidden"}"#.into())
            }
        })
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

const ALL: [&str; 5] = [
    "roms.read",
    "platforms.read",
    "collections.read",
    "collections.write",
    "tasks.run",
];

#[test]
fn testing_the_connection_reports_the_version_the_user_and_each_missing_scope() {
    let romm = StubRomm::like_romm(
        "5.4.1",
        "rmm_good",
        &["me.read", "roms.read", "platforms.read", "tasks.run"],
        false,
    );
    let check = RommClient::new(&romm.url, Some("rmm_good"))
        .unwrap()
        .check();
    assert_eq!(
        check,
        RommCheck {
            version: Some("5.4.1".into()),
            version_tested: true,
            user: Some(RommUser {
                username: "svc".into(),
                admin: false,
            }),
            missing_scopes: vec!["collections.read".into(), "collections.write".into()],
            failure: None,
        }
    );
    let seen = romm.seen();
    assert!(
        seen.iter()
            .filter(|r| r.path != "/api/heartbeat")
            .all(|r| r.authorization.as_deref() == Some("Bearer rmm_good")),
        "{seen:?}"
    );
    let write = seen
        .iter()
        .find(|r| r.method == "POST")
        .expect("collections.write is probed");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&write.body).unwrap(),
        serde_json::json!({"rom_ids": []}),
        "the probe adds nothing to any collection"
    );
}

#[test]
fn an_untested_version_and_a_token_that_cant_read_its_user_are_reported_but_not_failures() {
    let romm = StubRomm::like_romm("5.5.0-alpha.1", "rmm_good", &ALL, true);
    let check = RommClient::new(&format!("{}/", romm.url), Some("rmm_good"))
        .unwrap()
        .check();
    assert_eq!(check.version.as_deref(), Some("5.5.0-alpha.1"));
    assert!(!check.version_tested);
    assert_eq!(check.user, None, "the token lacks me.read");
    assert!(check.missing_scopes.is_empty(), "{check:?}");
    assert_eq!(check.failure, None);

    let admin = StubRomm::like_romm(
        "5.4.0",
        "rmm_good",
        &[&ALL[..], &["me.read"]].concat(),
        true,
    );
    let check = RommClient::new(&admin.url, Some("rmm_good"))
        .unwrap()
        .check();
    assert_eq!(
        check.user,
        Some(RommUser {
            username: "svc".into(),
            admin: true
        })
    );
}

#[test]
fn a_refused_token_a_missing_one_or_an_unreachable_romm_stops_the_check_and_says_why() {
    let romm = StubRomm::like_romm("5.4.1", "rmm_good", &ALL, false);
    let refused = RommClient::new(&romm.url, Some("rmm_wrong-secret"))
        .unwrap()
        .check();
    assert_eq!(refused.version.as_deref(), Some("5.4.1"));
    let why = refused.failure.expect("a refused token fails the check");
    assert!(why.contains("refused the token"), "{why}");
    assert!(!why.contains("rmm_wrong-secret"), "{why}");

    let missing = RommClient::new(&romm.url, None).unwrap().check();
    assert_eq!(missing.version.as_deref(), Some("5.4.1"));
    assert!(missing.failure.unwrap().contains("token"));

    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let gone = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let unreachable = RommClient::new(&gone, Some("rmm_good")).unwrap().check();
    assert_eq!(unreachable.version, None);
    assert!(
        unreachable
            .failure
            .as_deref()
            .is_some_and(|f| f.contains("can't be reached")),
        "{unreachable:?}"
    );

    let html = StubRomm::start(|_| (200, "<html>not RomM</html>".into()));
    let not_romm = RommClient::new(&html.url, Some("rmm_good"))
        .unwrap()
        .check();
    assert!(not_romm.failure.unwrap().contains("doesn't read"));
}

#[test]
fn a_request_gives_up_at_the_client_timeout_with_a_typed_error() {
    let slow = StubRomm::start(|_| {
        std::thread::sleep(Duration::from_secs(5));
        (200, "{}".into())
    });
    let client = RommClient::new(&slow.url, Some("rmm_good"))
        .unwrap()
        .with_timeout(Duration::from_millis(200));
    let started = Instant::now();
    match client.get::<serde_json::Value>("/api/heartbeat") {
        Err(RommError::Unreachable(_)) => {}
        other => panic!("{other:?}"),
    }
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn the_client_refuses_an_address_it_must_not_send_a_token_to() {
    for url in [
        "ftp://romm.example",
        "https://user:pass@romm.example",
        "https://romm.example/?x=1",
        "https://romm.no-intro.org",
    ] {
        assert!(
            matches!(
                RommClient::new(url, Some("rmm_t")),
                Err(RommError::Address(_))
            ),
            "{url}"
        );
    }
}

mod scans {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::common::*;
    use super::*;
    use title_select_api::build::{AddLibraryFolder, LibraryFolder, PlanCleanup};
    use title_select_api::romm::{RommPlatform, RommScanOutcome, RommUpdate};
    use title_select_api::selection::{
        CoverageDrift, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
        SelectionSummary, Winner,
    };
    use title_select_api::settings::{SecretUpdate, SettingsUpdate};
    use title_select_api::{Job, JobKind, JobState};
    use title_select_store::config::Config;
    use title_select_store::romm::{ScanStop, ScanTiming};
    use title_select_store::{Store, Workers};

    const SET: &str = "Test - System";
    const PROFILE: &str = "Mine";
    const WORKERS: Workers = Workers {
        readers: 1,
        hashers: 1,
    };

    /// Short enough that no test waits long, and every step still has room to happen.
    const FAST: ScanTiming = ScanTiming {
        poll: Duration::from_millis(10),
        backoff: Duration::from_millis(10),
        backoff_max: Duration::from_millis(40),
        window: Duration::from_secs(5),
        request: Duration::from_secs(5),
    };

    /// A store with an audited Source Set of two Games, and a writable Root `play` that RomM mounts as its library.
    struct Env {
        _tmp: tempfile::TempDir,
        store: Arc<Store>,
        config: Config,
        sha256: String,
    }

    impl Env {
        fn new() -> Env {
            let tmp = tempfile::tempdir().unwrap();
            let (sets, play) = (tmp.path().join("roots/sets"), tmp.path().join("roots/play"));
            std::fs::create_dir_all(sets.join(SET)).unwrap();
            std::fs::create_dir_all(&play).unwrap();
            std::fs::create_dir_all(tmp.path().join("in")).unwrap();
            let config = Config::parse(&format!(
                "[roots.sets]\npath = {:?}\n\n[roots.play]\npath = {:?}\nwritable = true\n",
                sets.display().to_string(),
                play.display().to_string(),
            ))
            .unwrap();
            let store = Store::open(&tmp.path().join("store")).unwrap();
            let dat = write_dat_version(
                &tmp.path().join("in"),
                "Test - System (1).dat",
                "20260101-000000",
                &[
                    ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
                    ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
                ],
            );
            let (revision, _) = store.import_dat(&dat, "No-Intro", None).unwrap();
            store
                .add_source_set(&config, "sets", SET, "No-Intro/Test - System")
                .unwrap();
            store
                .save_profile(
                    PROFILE,
                    &SelectionProfile::default(),
                    None,
                    "Operator",
                    &[],
                    WORKERS,
                )
                .unwrap();
            write_file(&sets.join(SET).join("Pong (USA).a26"), b"pong");
            write_file(&sets.join(SET).join("Combat (USA).a26"), b"combat");
            store
                .audit_source_set(
                    &config,
                    SET,
                    None,
                    title_select_engine::AuditEvidence::Incremental,
                )
                .unwrap();
            Env {
                _tmp: tmp,
                store: Arc::new(store),
                config,
                sha256: revision.sha256,
            }
        }

        /// Turns RomM on at `url`, mounting the Root `root` as its library.
        fn romm(&self, enabled: bool, url: &str, root: &str) {
            self.romm_with(enabled, url, SecretUpdate::Set("rmm_good".into()), root);
        }

        fn romm_with(&self, enabled: bool, url: &str, token: SecretUpdate, root: &str) {
            self.store
                .update_settings(
                    &self.config,
                    &SettingsUpdate {
                        romm: Some(RommUpdate {
                            enabled,
                            base_url: Some(url.into()),
                            token,
                            root: Some(root.into()),
                        }),
                        ..SettingsUpdate::default()
                    },
                    "Operator",
                )
                .unwrap();
        }

        fn folder(&self, path: &str) -> LibraryFolder {
            self.store
                .add_library_folder(
                    &self.config,
                    &AddLibraryFolder {
                        root: "play".into(),
                        path: Some(path.into()),
                        template: None,
                        source_set: SET.into(),
                        profile: PROFILE.into(),
                    },
                )
                .unwrap()
        }

        /// Records a Selection of every Game, then plans, approves and runs a Build of it into `folder`.
        fn build(&self, folder: &LibraryFolder) -> Job {
            self.build_of(folder, &["Pong (USA)", "Combat (USA)"])
        }

        /// Records a Selection of the Games `titles`, then plans, approves and runs a Build of it into `folder`.
        fn build_of(&self, folder: &LibraryFolder, titles: &[&str]) -> Job {
            self.store
                .enqueue_selection(SET, None, PROFILE, None, "Operator", WORKERS)
                .unwrap();
            let job = self.store.claim_job().unwrap().unwrap();
            let groups = titles.iter().map(|t| group(t)).collect();
            let selection = self
                .store
                .record_selection(job.id, &selection(&self.sha256, groups))
                .unwrap()
                .selection
                .unwrap();
            let plan = self
                .store
                .plan_build(&self.config, folder.id, selection)
                .unwrap();
            let job = self
                .store
                .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
                .unwrap();
            self.store.claim_job().unwrap().unwrap();
            let job = self
                .store
                .run_build_job(&self.config, job.id, None)
                .unwrap();
            assert_eq!(job.state, JobState::Succeeded, "{job:?}");
            job
        }

        /// Plans, approves and runs the cleanup `request` of `folder`.
        fn cleanup(&self, folder: &LibraryFolder, request: PlanCleanup) -> Job {
            let plan = self
                .store
                .plan_cleanup(&self.config, folder.id, &request)
                .unwrap();
            assert!(plan.approvable(), "{:?}", plan.content.blockers);
            let job = self
                .store
                .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
                .unwrap();
            self.store.claim_job().unwrap().unwrap();
            self.store
                .run_build_job(&self.config, job.id, None)
                .unwrap()
        }

        fn dir(&self, folder: &str) -> std::path::PathBuf {
            self._tmp.path().join("roots/play").join(folder)
        }

        /// Requests the scan the Job `job` owes RomM and follows it to its end. Returns the Job as each step left it.
        fn scan(&self, job: &Job, timing: &ScanTiming) -> Vec<Job> {
            self.scan_until(job, timing, &ScanStop::default())
        }

        /// As [`Env::scan`], until `stop` is signalled.
        fn scan_until(&self, job: &Job, timing: &ScanTiming, stop: &ScanStop) -> Vec<Job> {
            let mut seen = vec![];
            let ended = Store::request_romm_scan(
                &Arc::downgrade(&self.store),
                &self.config,
                job.id,
                timing,
                stop,
                &mut |j: &Job| seen.push(j.clone()),
            )
            .unwrap();
            assert_eq!(ended.as_ref(), seen.last(), "the end is the last step told");
            seen
        }
    }

    fn group(title: &str) -> SelectionGroup {
        SelectionGroup {
            group: title.to_lowercase(),
            unresolved: false,
            winners: vec![Winner {
                title: title.into(),
                rule: Rule {
                    stage: "Parent selection".into(),
                    reference: Some("REF0001".into()),
                    message: "m".into(),
                },
                unresolved: false,
            }],
            alternatives: vec![],
            exclusions: vec![],
            choice: None,
            drift: false,
        }
    }

    fn selection(sha256: &str, groups: Vec<SelectionGroup>) -> Selection {
        Selection {
            pins: SelectionPins::default(),
            dat_sha256: sha256.into(),
            system: SET.into(),
            system_settings: false,
            summary: SelectionSummary::default(),
            coverage_drift: CoverageDrift::default(),
            groups,
            stale_choices: BTreeMap::new(),
        }
    }

    /// A stub RomM that accepts a scan after `busy` answers of 409, as task `t1`, and reports it `status` from its
    /// second poll on; or answers every scan request with `refuse`.
    fn scanning(busy: usize, refuse: Option<u16>, status: &'static str) -> StubRomm {
        let refused = AtomicUsize::new(0);
        let polls = AtomicUsize::new(0);
        StubRomm::start(move |r| {
            if r.authorization.as_deref() != Some("Bearer rmm_good") {
                return (401, r#"{"detail": "Not authenticated"}"#.into());
            }
            match (r.method.as_str(), r.path.as_str()) {
                ("POST", "/api/tasks/scan") => {
                    if let Some(status) = refuse {
                        return (status, r#"{"detail": "No scan worker"}"#.into());
                    }
                    if refused.fetch_add(1, Ordering::SeqCst) < busy {
                        return (409, r#"{"detail": "A scan is already running"}"#.into());
                    }
                    (
                        202,
                        r#"{"task_name": "Scan", "task_id": "t1", "status": "queued", "created_at": "x"}"#
                            .into(),
                    )
                }
                ("GET", "/api/tasks/t1") => {
                    let status = match polls.fetch_add(1, Ordering::SeqCst) {
                        0 => "started",
                        _ => status,
                    };
                    (
                        200,
                        format!(
                            r#"{{"task_id": "t1", "task_type": "scan", "status": "{status}"}}"#
                        ),
                    )
                }
                _ => (404, r#"{"detail": "Not Found"}"#.into()),
            }
        })
    }

    fn requests(romm: &StubRomm, path: &str) -> Vec<Seen> {
        romm.seen().into_iter().filter(|r| r.path == path).collect()
    }

    #[test]
    fn a_build_that_changed_a_mapped_folder_requests_a_quick_scan_of_its_platform_and_records_it_finished()
     {
        let env = Env::new();
        let romm = scanning(0, None, "finished");
        env.romm(true, &romm.url, "play");
        let folder = env.folder("gba");
        let job = env.build(&folder);

        let steps = env.scan(&job, &FAST);

        let posted = requests(&romm, "/api/tasks/scan");
        assert_eq!(posted.len(), 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&posted[0].body).unwrap(),
            serde_json::json!({"type": "quick", "platform_fs_slugs": ["gba"]})
        );
        let outcomes: Vec<_> = steps
            .iter()
            .map(|j| j.romm_scan.as_ref().unwrap().outcome)
            .collect();
        assert_eq!(
            outcomes,
            [RommScanOutcome::Requested, RommScanOutcome::Finished]
        );
        let ended = env.store.job(job.id).unwrap();
        assert_eq!(ended.state, JobState::Succeeded);
        assert!(
            ended.version > job.version,
            "each step is a change a client sees"
        );
        let scan = ended.romm_scan.unwrap();
        assert_eq!(
            scan.platform,
            RommPlatform::Mapped {
                fs_slug: "gba".into()
            }
        );
        assert_eq!(
            (
                scan.task_id.as_deref(),
                scan.detail,
                scan.ended_at.is_some()
            ),
            (Some("t1"), None, true)
        );
    }

    #[test]
    fn a_scan_refused_while_another_runs_is_asked_again_and_followed_to_its_end() {
        let env = Env::new();
        let romm = scanning(2, None, "finished");
        env.romm(true, &romm.url, "play");
        let job = env.build(&env.folder("gba"));

        env.scan(&job, &FAST);

        assert_eq!(requests(&romm, "/api/tasks/scan").len(), 3);
        let ended = env.store.job(job.id).unwrap();
        assert_eq!(ended.romm_scan.unwrap().outcome, RommScanOutcome::Finished);
        assert_eq!(ended.state, JobState::Succeeded);
    }

    #[test]
    fn a_failed_timed_out_or_unavailable_scan_is_a_warning_and_the_job_still_succeeds() {
        let env = Env::new();
        let window = ScanTiming {
            window: Duration::from_millis(300),
            ..FAST
        };
        for (case, (romm, timing, expected)) in [
            (scanning(0, None, "failed"), FAST, RommScanOutcome::Failed),
            (
                scanning(0, None, "started"),
                window,
                RommScanOutcome::TimedOut,
            ),
            (
                scanning(usize::MAX, None, "finished"),
                window,
                RommScanOutcome::TimedOut,
            ),
            (
                scanning(0, Some(503), "finished"),
                FAST,
                RommScanOutcome::Unavailable,
            ),
            (
                scanning(0, Some(403), "finished"),
                FAST,
                RommScanOutcome::Error,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            env.romm(true, &romm.url, "play");
            let folder = env.folder(&format!("gba-{case}"));
            let job = env.build(&folder);
            let started = Instant::now();

            env.scan(&job, &timing);

            assert!(started.elapsed() < Duration::from_secs(4));
            let ended = env.store.job(job.id).unwrap();
            assert_eq!(ended.state, JobState::Succeeded, "{expected:?}");
            let scan = ended.romm_scan.unwrap();
            assert_eq!(scan.outcome, expected);
            assert!(scan.outcome.is_warning());
            let detail = scan.detail.expect("a warning says why");
            assert!(!detail.contains("rmm_good"), "{detail}");
            assert!(scan.ended_at.is_some());
        }
    }

    #[test]
    fn no_scan_is_requested_when_romm_is_off_nothing_changed_or_the_folder_is_not_one_romm_scans() {
        let env = Env::new();
        let romm = scanning(0, None, "finished");
        let none = |env: &Env, job: &Job| {
            let ended = Store::request_romm_scan(
                &Arc::downgrade(&env.store),
                &env.config,
                job.id,
                &FAST,
                &ScanStop::default(),
                &mut |_: &Job| panic!("nothing is recorded"),
            )
            .unwrap();
            assert_eq!(ended, None);
            assert_eq!(env.store.job(job.id).unwrap().romm_scan, None);
        };

        env.romm(false, &romm.url, "play");
        let folder = env.folder("gba");
        let built = env.build(&folder);
        none(&env, &built);

        env.romm(true, &romm.url, "play");
        let unchanged = env.build(&folder);
        none(&env, &unchanged);

        env.romm(true, &romm.url, "sets");
        let outside = env.build(&env.folder("n64"));
        none(&env, &outside);

        let audit = env
            .store
            .enqueue_audit(
                SET,
                None,
                "Operator",
                WORKERS,
                title_select_engine::AuditEvidence::Incremental,
            )
            .unwrap();
        env.store.claim_job().unwrap().unwrap();
        let audit = env
            .store
            .run_audit_job(&env.config, audit.id, None)
            .unwrap();
        assert_eq!(audit.kind, JobKind::Audit);
        none(&env, &audit);

        assert!(romm.seen().is_empty(), "{:?}", romm.seen());
    }

    #[test]
    fn a_changed_folder_nested_under_the_romm_root_is_a_warning_with_no_request() {
        let env = Env::new();
        let romm = scanning(0, None, "finished");
        env.romm(true, &romm.url, "play");
        let job = env.build(&env.folder("arcade/neogeo"));

        let steps = env.scan(&job, &FAST);

        assert_eq!(steps.len(), 1);
        let scan = env.store.job(job.id).unwrap().romm_scan.unwrap();
        assert_eq!(
            (scan.platform, scan.outcome),
            (RommPlatform::Nested, RommScanOutcome::Unmapped)
        );
        assert!(scan.detail.is_some());
        assert!(romm.seen().is_empty(), "{:?}", romm.seen());
    }

    /// A scan still followed when the service stopped is never followed again: the next start records that it
    /// stopped waiting, so the Job doesn't show it scanning forever.
    #[test]
    fn a_scan_followed_when_the_service_stopped_is_recorded_as_no_longer_followed() {
        let env = Env::new();
        let romm = scanning(0, None, "started");
        env.romm(true, &romm.url, "play");
        let job = env.build(&env.folder("gba"));
        let timing = ScanTiming {
            window: Duration::from_millis(500),
            ..FAST
        };
        let following = std::sync::Barrier::new(2);

        let recovered = std::thread::scope(|s| {
            s.spawn(|| {
                let mut first = true;
                Store::request_romm_scan(
                    &Arc::downgrade(&env.store),
                    &env.config,
                    job.id,
                    &timing,
                    &ScanStop::default(),
                    &mut |_: &Job| {
                        if std::mem::take(&mut first) {
                            following.wait();
                        }
                    },
                )
                .unwrap();
            });
            following.wait();
            let recovered = env.store.recover_romm_scans().unwrap();
            (recovered, env.store.job(job.id).unwrap().romm_scan.unwrap())
        });

        let (recovered, scan) = recovered;
        assert_eq!(recovered, [job.id]);
        assert_eq!(scan.outcome, RommScanOutcome::Stopped);
        assert!(scan.ended_at.is_some());
        assert!(env.store.recover_romm_scans().unwrap().is_empty());
    }

    /// A stub RomM that accepts a scan as task `t1` and reports it started, or finished to the token `rmm_new`. It
    /// holds its answer to the first request for `path` until the test has done what it does then.
    fn held(path: &'static str, first: u16) -> (StubRomm, Hold) {
        let (reached, wait_reached) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let (reached, released) = (Mutex::new(reached), Mutex::new(released));
        let once = AtomicUsize::new(0);
        let romm = StubRomm::start(move |r| {
            let new = r.authorization.as_deref() == Some("Bearer rmm_new");
            if !new && r.authorization.as_deref() != Some("Bearer rmm_good") {
                return (401, r#"{"detail": "Not authenticated"}"#.into());
            }
            if r.path == path && once.fetch_add(1, Ordering::SeqCst) == 0 {
                reached.lock().unwrap().send(()).unwrap();
                released.lock().unwrap().recv().unwrap();
                if first != 200 {
                    return (first, r#"{"detail": "A scan is already running"}"#.into());
                }
            }
            match r.path.as_str() {
                "/api/tasks/scan" => (202, r#"{"task_id": "t1", "status": "queued"}"#.into()),
                "/api/tasks/t1" => {
                    let status = if new { "finished" } else { "started" };
                    (200, format!(r#"{{"task_id": "t1", "status": "{status}"}}"#))
                }
                _ => (404, r#"{"detail": "Not Found"}"#.into()),
            }
        });
        (romm, Hold(wait_reached, release))
    }

    /// The test's side of [`held`]: it waits for the held request, then lets the stub answer it.
    struct Hold(std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>);

    impl Hold {
        fn when_reached(&self, then: impl FnOnce()) {
            self.0.recv().unwrap();
            then();
            self.1.send(()).unwrap();
        }
    }

    /// RomM turned off, or its address or root changed, while a scan is followed: Title Select stops following it
    /// and sends RomM nothing more, the saved token least of all.
    #[test]
    fn turning_romm_off_or_changing_its_address_or_root_stops_following_the_scan() {
        let env = Env::new();
        let elsewhere = scanning(0, None, "finished");
        type Change<'a> = Box<dyn Fn(&Env, &str) + 'a>;
        let changes: [(&str, &str, Change); 4] = [
            (
                "/api/tasks/scan",
                "off while busy",
                Box::new(|env, url| env.romm(false, url, "play")),
            ),
            (
                "/api/tasks/t1",
                "off",
                Box::new(|env, url| env.romm(false, url, "play")),
            ),
            (
                "/api/tasks/t1",
                "address",
                Box::new(|env, _| env.romm(true, &elsewhere.url, "play")),
            ),
            (
                "/api/tasks/t1",
                "root",
                Box::new(|env, url| env.romm(true, url, "sets")),
            ),
        ];
        for (case, (path, name, change)) in changes.into_iter().enumerate() {
            let (romm, hold) = held(path, if path == "/api/tasks/scan" { 409 } else { 200 });
            env.romm(true, &romm.url, "play");
            let job = env.build(&env.folder(&format!("gba-{case}")));

            let steps = std::thread::scope(|s| {
                let scanning = s.spawn(|| env.scan(&job, &FAST));
                hold.when_reached(|| change(&env, &romm.url));
                scanning.join().unwrap()
            });

            let scan = steps.last().unwrap().romm_scan.clone().unwrap();
            assert_eq!(scan.outcome, RommScanOutcome::Stopped, "{name}: {scan:?}");
            assert!(scan.detail.is_some(), "{name}");
            assert_eq!(
                romm.seen().len(),
                1 + usize::from(path == "/api/tasks/t1"),
                "{name}"
            );
            assert!(elsewhere.seen().is_empty(), "{name}");
        }
    }

    /// A token replaced at the same address while a scan is followed is the one the next request sends.
    #[test]
    fn a_token_replaced_while_a_scan_is_followed_is_used_from_the_next_request() {
        let env = Env::new();
        let (romm, hold) = held("/api/tasks/t1", 200);
        env.romm(true, &romm.url, "play");
        let job = env.build(&env.folder("gba"));

        let steps = std::thread::scope(|s| {
            let scanning = s.spawn(|| env.scan(&job, &FAST));
            hold.when_reached(|| {
                env.romm_with(true, &romm.url, SecretUpdate::Set("rmm_new".into()), "play")
            });
            scanning.join().unwrap()
        });

        let scan = steps.last().unwrap().romm_scan.clone().unwrap();
        assert_eq!(scan.outcome, RommScanOutcome::Finished, "{scan:?}");
        let last = romm.seen().pop().unwrap();
        assert_eq!(last.authorization.as_deref(), Some("Bearer rmm_new"));
    }

    /// Signalled to stop, as when the service stops, a scan's follower stops waiting at once and records the scan
    /// as no longer followed, as the next start would; one signalled before it starts asks RomM for nothing.
    #[test]
    fn a_stop_ends_the_wait_at_once_and_records_the_scan_no_longer_followed() {
        let env = Env::new();
        let romm = scanning(0, None, "started");
        env.romm(true, &romm.url, "play");
        let long = ScanTiming {
            poll: Duration::from_secs(30),
            window: Duration::from_secs(600),
            ..FAST
        };
        let job = env.build(&env.folder("gba"));
        let stop = ScanStop::default();
        let started = Instant::now();

        let steps = std::thread::scope(|s| {
            let scanning = s.spawn(|| env.scan_until(&job, &long, &stop));
            while requests(&romm, "/api/tasks/t1").is_empty() {
                std::thread::sleep(Duration::from_millis(5));
            }
            stop.stop();
            scanning.join().unwrap()
        });

        assert!(started.elapsed() < Duration::from_secs(5));
        let scan = steps.last().unwrap().romm_scan.clone().unwrap();
        assert_eq!(scan.outcome, RommScanOutcome::Stopped);
        assert!(scan.ended_at.is_some() && scan.detail.is_some());
        assert!(env.store.recover_romm_scans().unwrap().is_empty());

        let seen = romm.seen().len();
        let job = env.build_of(&env.folder("snes"), &["Pong (USA)"]);
        let steps = env.scan_until(&job, &long, &stop);
        assert_eq!(
            steps.last().unwrap().romm_scan.as_ref().unwrap().outcome,
            RommScanOutcome::Stopped
        );
        assert_eq!(romm.seen().len(), seen, "nothing is asked once stopped");
    }

    /// A stub RomM that accepts each connection, says so on the channel, and never answers.
    fn silent() -> (String, std::sync::mpsc::Receiver<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (accepted, told) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut open = vec![];
            for stream in listener.incoming() {
                open.push(stream);
                let _ = accepted.send(());
            }
        });
        (url, told)
    }

    /// While RomM leaves a request unanswered, the follower holds no Store: one stopped meanwhile is recorded as
    /// stopped once the request gives up, not as an error, and one whose Store is gone meanwhile records nothing.
    #[test]
    fn a_request_romm_never_answers_holds_no_store_and_a_stop_meanwhile_wins() {
        let env = Env::new();
        let (url, accepted) = silent();
        env.romm(true, &url, "play");
        let timing = ScanTiming {
            request: Duration::from_millis(300),
            ..FAST
        };
        let job = env.build(&env.folder("gba"));
        let stop = ScanStop::default();

        let steps = std::thread::scope(|s| {
            let scanning = s.spawn(|| env.scan_until(&job, &timing, &stop));
            accepted.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_eq!(
                Arc::strong_count(&env.store),
                1,
                "the store isn't held while RomM is asked"
            );
            stop.stop();
            scanning.join().unwrap()
        });

        let scan = steps.last().unwrap().romm_scan.clone().unwrap();
        assert_eq!(scan.outcome, RommScanOutcome::Stopped, "{scan:?}");
        assert!(scan.detail.unwrap().contains("isn't known"));

        let job = env.build(&env.folder("snes"));
        let Env {
            _tmp: tmp,
            store,
            config,
            ..
        } = env;
        let weak = Arc::downgrade(&store);
        let scanning = std::thread::spawn(move || {
            Store::request_romm_scan(
                &weak,
                &config,
                job.id,
                &timing,
                &ScanStop::default(),
                &mut |_: &Job| {},
            )
        });
        accepted.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(store);
        let reopened = Store::open(&tmp.path().join("store")).unwrap();
        assert_eq!(scanning.join().unwrap().unwrap(), None);
        assert_eq!(
            reopened.job(job.id).unwrap().romm_scan.unwrap().outcome,
            RommScanOutcome::Requested,
            "left for the next start to record as stopped"
        );
    }

    /// A build that only forgets the records of files already gone changed nothing RomM could see; one that keeps a
    /// file aside did.
    #[test]
    fn forgetting_files_already_gone_asks_for_no_scan_but_keeping_one_aside_does() {
        let env = Env::new();
        let romm = scanning(0, None, "finished");
        env.romm(true, &romm.url, "play");
        let (gone, kept) = (env.folder("gba"), env.folder("snes"));
        env.build(&gone);
        env.build(&kept);
        std::fs::remove_file(env.dir("gba").join("Combat (USA).zip")).unwrap();

        let forgot = env.build_of(&gone, &["Pong (USA)"]);
        let kept_aside = env.build_of(&kept, &["Pong (USA)"]);

        assert!(
            env.scan(&forgot, &FAST).is_empty(),
            "nothing changed on disk"
        );
        assert_eq!(
            env.scan(&kept_aside, &FAST)
                .last()
                .unwrap()
                .romm_scan
                .as_ref()
                .unwrap()
                .outcome,
            RommScanOutcome::Finished
        );
    }

    /// A cleanup's repack that kept the old copy aside and then failed to put the new one in place still changed the
    /// folder: it lost a file.
    #[test]
    fn a_repack_that_kept_the_old_copy_aside_and_then_failed_still_asks_for_a_scan() {
        let env = Env::new();
        let romm = scanning(0, None, "finished");
        env.romm(true, &romm.url, "play");
        let folder = env.folder("gba");
        env.build(&folder);
        let combat = env.dir("gba").join("Combat (USA).zip");
        write_stored_zip(&combat, &[("Combat (USA).a26", b"combat")]);
        let blocked = combat.clone();
        env.store.hook_faults(Some(Arc::new(move |f| {
            if matches!(f, title_select_store::Fault::AfterKeep(_)) && !blocked.exists() {
                std::fs::create_dir(&blocked).unwrap();
            }
        })));

        let job = env.cleanup(
            &folder,
            PlanCleanup {
                repack: vec!["Combat (USA).zip".into()],
                ..PlanCleanup::default()
            },
        );
        env.store.hook_faults(None);

        assert!(
            combat.is_dir(),
            "the new copy was never put in place: {job:?}"
        );
        let steps = env.scan(&job, &FAST);
        assert_eq!(
            steps.last().map(|j| j.romm_scan.as_ref().unwrap().outcome),
            Some(RommScanOutcome::Finished)
        );
    }
}
