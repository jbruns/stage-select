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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::common::*;
    use super::*;
    use title_select_api::build::{AddLibraryFolder, LibraryFolder};
    use title_select_api::romm::{RommPlatform, RommScanOutcome, RommUpdate};
    use title_select_api::selection::{
        CoverageDrift, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
        SelectionSummary, Winner,
    };
    use title_select_api::settings::{SecretUpdate, SettingsUpdate};
    use title_select_api::{Job, JobKind, JobState};
    use title_select_store::config::Config;
    use title_select_store::romm::ScanTiming;
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
    };

    /// A store with an audited Source Set of two Games, and a writable Root `play` that RomM mounts as its library.
    struct Env {
        _tmp: tempfile::TempDir,
        store: Store,
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
                store,
                config,
                sha256: revision.sha256,
            }
        }

        /// Turns RomM on at `url`, mounting the Root `root` as its library.
        fn romm(&self, enabled: bool, url: &str, root: &str) {
            self.store
                .update_settings(
                    &self.config,
                    &SettingsUpdate {
                        romm: Some(RommUpdate {
                            enabled,
                            base_url: Some(url.into()),
                            token: SecretUpdate::Set("rmm_good".into()),
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
            self.store
                .enqueue_selection(SET, None, PROFILE, None, "Operator", WORKERS)
                .unwrap();
            let job = self.store.claim_job().unwrap().unwrap();
            let groups = ["Pong (USA)", "Combat (USA)"].map(group).to_vec();
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

        /// Requests the scan the Job `job` owes RomM and follows it to its end. Returns the Job as each step left it.
        fn scan(&self, job: &Job, timing: &ScanTiming) -> Vec<Job> {
            let mut seen = vec![];
            let ended = self
                .store
                .request_romm_scan(&self.config, job.id, timing, &mut |j: &Job| {
                    seen.push(j.clone())
                })
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
            let ended = env
                .store
                .request_romm_scan(&env.config, job.id, &FAST, &mut |_: &Job| {
                    panic!("nothing is recorded")
                })
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
                env.store
                    .request_romm_scan(&env.config, job.id, &timing, &mut |_: &Job| {
                        if std::mem::take(&mut first) {
                            following.wait();
                        }
                    })
                    .unwrap();
            });
            following.wait();
            env.store.recover_romm_scans().unwrap()
        });

        assert_eq!(recovered, [job.id]);
        let scan = env.store.job(job.id).unwrap().romm_scan.unwrap();
        assert_eq!(scan.outcome, RommScanOutcome::TimedOut);
        assert!(scan.ended_at.is_some());
        assert!(env.store.recover_romm_scans().unwrap().is_empty());
    }
}
