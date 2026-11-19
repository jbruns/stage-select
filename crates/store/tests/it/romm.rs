//! The RomM client, its connection test and the scan a Build requests, against a stub RomM on a local port that
//! answers as RomM 5.4.1 does (research 0136). No test sends a request to a real RomM.

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
    // It answers once the test ends: only the client's timeout ends the request before then.
    let (_answer, held) = std::sync::mpsc::channel::<()>();
    let held = Mutex::new(held);
    let slow = StubRomm::start(move |_| {
        let _ = held.lock().unwrap().recv();
        (200, "{}".into())
    });
    let client = RommClient::new(&slow.url, Some("rmm_good"))
        .unwrap()
        .with_timeout(Duration::from_millis(200));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(client.get::<serde_json::Value>("/api/heartbeat"));
    });
    match rx
        .recv_timeout(Duration::from_secs(30))
        .expect("it gave up")
    {
        Err(RommError::Unreachable(_)) => {}
        other => panic!("{other:?}"),
    }
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

    use super::*;
    use crate::common::*;
    use title_select_api::build::{AddLibraryFolder, LibraryFolder, PlanCleanup};
    use title_select_api::romm::{RommPlatform, RommScanOutcome, RommUpdate};
    use title_select_api::selection::{
        CoverageDrift, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
        SelectionSummary, Winner,
    };
    use title_select_api::settings::{SecretUpdate, SettingsUpdate};
    use title_select_api::storage::SystemStorage;
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
            Env::of(
                "No-Intro",
                vec![
                    ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
                    ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
                ],
                None,
            )
        }

        /// A store with an audited Source Set of `games` from the DAT provider `provider`, each a file of its one
        /// ROM or a folder of its ROMs, stored in play libraries as `storage` says.
        pub(super) fn of(
            provider: &str,
            games: Vec<(&'static str, Vec<RomSpec>)>,
            storage: Option<SystemStorage>,
        ) -> Env {
            Env::headered(provider, games, storage, None)
        }

        /// As [`Env::of`], with a DAT naming the Header Rule `header.0`, whose dumps in the Source Set start with the
        /// Header `header.1`.
        pub(super) fn headered(
            provider: &str,
            games: Vec<(&'static str, Vec<RomSpec>)>,
            storage: Option<SystemStorage>,
            header: Option<(&str, &[u8])>,
        ) -> Env {
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
            let dat = match header {
                Some((rule, _)) => {
                    write_dat_with_header_from(&tmp.path().join("in"), rule, provider, &games)
                }
                None => write_dat_from(
                    &tmp.path().join("in"),
                    "Test - System (1).dat",
                    "20260101-000000",
                    provider,
                    &games,
                ),
            };
            let (revision, _) = store.import_dat(&dat, None).unwrap();
            let system = format!("{provider}/{SET}");
            store.add_source_set(&config, "sets", SET, &system).unwrap();
            if let Some(storage) = storage {
                let mut policy = store.storage_policy().unwrap();
                policy.play_libraries.systems.insert(system, storage);
                store.set_storage_policy(&policy, "Operator").unwrap();
            }
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
            let prefix = header.map(|(_, bytes)| bytes).unwrap_or_default();
            for (game, roms) in &games {
                match &roms[..] {
                    [one] => write_file(
                        &sets.join(SET).join(one.name),
                        &[prefix, &one.payload].concat(),
                    ),
                    _ => write_loose_folder(&sets.join(SET), game, roms),
                }
            }
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

            env.scan(&job, &timing);

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
        // Waiting out a poll would take a minute.
        let long = ScanTiming {
            poll: Duration::from_secs(60),
            window: Duration::from_secs(600),
            ..FAST
        };
        let job = env.build(&env.folder("gba"));
        let stop = ScanStop::default();

        let steps = std::thread::scope(|s| {
            let (ended, end) = std::sync::mpsc::channel();
            let (env, job, long, stop) = (&env, &job, &long, &stop);
            let scanning = s.spawn(move || {
                let steps = env.scan_until(job, long, stop);
                let _ = ended.send(());
                steps
            });
            let deadline = Instant::now() + Duration::from_secs(30);
            while requests(&romm, "/api/tasks/t1").is_empty() {
                assert!(Instant::now() < deadline, "the scan was never polled");
                std::thread::sleep(Duration::from_millis(5));
            }
            stop.stop();
            end.recv_timeout(Duration::from_secs(30))
                .expect("the stop ended the wait");
            scanning.join().unwrap()
        });

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
            accepted.recv_timeout(Duration::from_secs(30)).unwrap();
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
        accepted.recv_timeout(Duration::from_secs(30)).unwrap();
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

    /// Comparing RomM's view of a Library Folder with the files its Builds made, after a scan (#143).
    mod reconcile {
        use super::*;
        use serde_json::{Value, json};
        use sha1::Digest;
        use title_select_api::romm::{RommFlag, RommReconcileCounts, RommReconciliation};
        use title_select_api::storage::{DiscLayout, StorageFormat};

        /// RomM's internal id for the platform the tests map, which isn't its `fs_slug`.
        const PLATFORM: i64 = 7;

        fn sha1(bytes: &[u8]) -> String {
            sha1::Sha1::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        }

        /// A file of a ROM as RomM 5.4.1 lists it in `files[]`: `path` is under the platform folder `roms/<fs_slug>`.
        fn file(fs_slug: &str, path: &str, sha1: &str) -> Value {
            let full = format!("roms/{fs_slug}/{path}");
            let (dir, name) = full.rsplit_once('/').unwrap();
            json!({
                "id": 100, "rom_id": 1, "file_name": name, "file_path": dir, "full_path": full,
                "file_size_bytes": 4, "is_top_level": !path.contains('/'), "crc_hash": "", "md5_hash": "",
                "sha1_hash": sha1, "chd_sha1_hash": "", "archive_members": null, "category": null,
            })
        }

        /// A zip as RomM lists it: its own composite hash, and each member's.
        fn zip(fs_slug: &str, name: &str, members: &[(&str, &[u8])]) -> Value {
            let mut file = file(fs_slug, name, "c0ffee");
            file["archive_members"] = members
                .iter()
                .map(|(n, payload)| {
                    json!({"name": n, "size": payload.len(), "crc_hash": "", "md5_hash": "", "sha1_hash": sha1(payload)})
                })
                .collect();
            file
        }

        /// A ROM as RomM 5.4.1's `SimpleRomSchema` has it, with its files: `name` is the name a provider matched,
        /// and `None` for one no provider matched.
        fn rom_of(
            id: i64,
            fs_slug: &str,
            fs_name: &str,
            name: Option<&str>,
            files: Vec<Value>,
        ) -> Value {
            json!({
                "id": id, "platform_id": PLATFORM, "platform_fs_slug": fs_slug, "fs_name": fs_name,
                "fs_path": format!("roms/{fs_slug}"), "full_path": format!("roms/{fs_slug}/{fs_name}"),
                "name": name.unwrap_or(fs_name), "is_unidentified": name.is_none(), "is_identified": name.is_some(),
                "missing_from_fs": false, "files": files,
            })
        }

        /// A stub RomM whose platform `fs_slug` holds `roms`, paged as RomM pages `GET /api/roms`, that accepts a
        /// rescan as task `t9`.
        fn library(fs_slug: &str, roms: Vec<Value>) -> StubRomm {
            library_then(fs_slug, roms, |_| {})
        }

        /// [`library`], which does `first` with each request before it answers it.
        fn library_then(
            fs_slug: &str,
            roms: Vec<Value>,
            first: impl Fn(&Seen) + Send + Sync + 'static,
        ) -> StubRomm {
            let fs_slug = fs_slug.to_owned();
            StubRomm::start(move |r| {
                first(r);
                if r.authorization.as_deref() != Some("Bearer rmm_good") {
                    return (401, r#"{"detail": "Not authenticated"}"#.into());
                }
                let (path, query) = r.path.split_once('?').unwrap_or((&r.path, ""));
                let param = |name: &str| {
                    query
                        .split('&')
                        .filter_map(|p| p.split_once('='))
                        .filter(|(k, _)| *k == name)
                        .map(|(_, v)| v.to_owned())
                        .collect::<Vec<_>>()
                };
                match (r.method.as_str(), path) {
                    ("GET", "/api/platforms") => (
                        200,
                        json!([
                            {"id": 3, "slug": "nes", "fs_slug": "nes", "name": "NES"},
                            {"id": PLATFORM, "slug": "x", "fs_slug": fs_slug, "name": "X"},
                        ])
                        .to_string(),
                    ),
                    ("GET", "/api/roms") => {
                        if param("platform_ids") != [PLATFORM.to_string()] || param("with_files") != ["true"] {
                            return (200, json!({"items": [], "total": 0, "limit": 50, "offset": 0}).to_string());
                        }
                        let limit: usize = param("limit")[0].parse().unwrap();
                        let offset: usize = param("offset")[0].parse().unwrap();
                        let items: Vec<_> = roms.iter().skip(offset).take(limit).cloned().collect();
                        (
                            200,
                            json!({"items": items, "total": roms.len(), "limit": limit, "offset": offset,
                                   "char_index": {}, "rom_id_index": [], "filter_values": {}})
                            .to_string(),
                        )
                    }
                    ("POST", "/api/tasks/scan") => (
                        202,
                        r#"{"task_name": "Scan", "task_id": "t9", "status": "queued", "created_at": "x"}"#.into(),
                    ),
                    _ => (404, r#"{"detail": "Not Found"}"#.into()),
                }
            })
        }

        fn reconcile(env: &Env, folder: &LibraryFolder, page: usize) -> Option<RommReconciliation> {
            Store::reconcile_romm(
                &Arc::downgrade(&env.store),
                &env.config,
                folder.id,
                None,
                page,
                &FAST,
                &ScanStop::default(),
            )
            .unwrap()
        }

        /// The flags of each row, by target.
        fn flags(reconciled: &RommReconciliation) -> Vec<(&str, Vec<RommFlag>, Option<i64>)> {
            reconciled
                .rows
                .iter()
                .map(|r| (r.target.as_str(), r.flags.clone(), r.rom_id))
                .collect()
        }

        fn atari() -> Env {
            Env::of(
                "No-Intro",
                vec![
                    ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
                    ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
                    (
                        "Adventure (USA)",
                        vec![rom("Adventure (USA).a26", b"adventure")],
                    ),
                    (
                        "Breakout (USA)",
                        vec![rom("Breakout (USA).a26", b"breakout")],
                    ),
                    ("Outlaw (USA)", vec![rom("Outlaw (USA).a26", b"outlaw")]),
                ],
                None,
            )
        }

        const ATARI: [&str; 5] = [
            "Pong (USA)",
            "Combat (USA)",
            "Adventure (USA)",
            "Breakout (USA)",
            "Outlaw (USA)",
        ];

        #[test]
        fn each_disagreement_is_flagged_on_the_file_the_build_made_across_every_page() {
            let env = atari();
            let folder = env.folder("a26");
            env.build_of(&folder, &ATARI);
            let romm = library(
                "a26",
                vec![
                    rom_of(
                        1,
                        "a26",
                        "Pong (USA).zip",
                        Some("Pong"),
                        vec![zip("a26", "Pong (USA).zip", &[("Pong (USA).a26", b"pong")])],
                    ),
                    rom_of(
                        2,
                        "a26",
                        "Combat (USA).zip",
                        None,
                        vec![zip(
                            "a26",
                            "Combat (USA).zip",
                            &[("Combat (USA).a26", b"combat")],
                        )],
                    ),
                    rom_of(
                        3,
                        "a26",
                        "Adventure (USA).zip",
                        Some("Haunted House"),
                        vec![zip(
                            "a26",
                            "Adventure (USA).zip",
                            &[("Adventure (USA).a26", b"adventure")],
                        )],
                    ),
                    rom_of(
                        4,
                        "a26",
                        "Breakout (USA).zip",
                        Some("Breakout"),
                        vec![zip(
                            "a26",
                            "Breakout (USA).zip",
                            &[("Breakout (USA).a26", b"breakout!")],
                        )],
                    ),
                    // Made by no build: drift covers it, so it isn't reported.
                    rom_of(
                        5,
                        "a26",
                        "Stray (USA).zip",
                        None,
                        vec![zip(
                            "a26",
                            "Stray (USA).zip",
                            &[("Stray (USA).a26", b"stray")],
                        )],
                    ),
                ],
            );
            env.romm(true, &romm.url, "play");

            let reconciled =
                reconcile(&env, &folder, 2).expect("RomM is on and the folder is its platform");

            assert_eq!(reconciled.error, None);
            assert_eq!(
                flags(&reconciled),
                [
                    (
                        "Adventure (USA).zip",
                        vec![RommFlag::TitleMismatch],
                        Some(3)
                    ),
                    ("Breakout (USA).zip", vec![RommFlag::HashMismatch], Some(4)),
                    ("Combat (USA).zip", vec![RommFlag::Unidentified], Some(2)),
                    ("Outlaw (USA).zip", vec![RommFlag::HashMismatch], None),
                ]
            );
            assert_eq!(
                reconciled.counts,
                RommReconcileCounts {
                    files: 5,
                    clean: 1,
                    unidentified: 1,
                    title_mismatch: 1,
                    hash_mismatch: 2
                }
            );
            assert_eq!(
                reconciled.rows[0].romm_name.as_deref(),
                Some("Haunted House")
            );
            assert_eq!(reconciled.rows[0].game, "Adventure (USA)");
            let offsets: Vec<_> = romm
                .seen()
                .into_iter()
                .filter(|r| r.path.starts_with("/api/roms?"))
                .map(|r| {
                    let offset = r
                        .path
                        .split('&')
                        .find_map(|p| p.strip_prefix("offset="))
                        .unwrap()
                        .to_owned();
                    assert!(r.path.contains("limit=2"), "{}", r.path);
                    offset
                })
                .collect();
            assert_eq!(offsets, ["0", "2", "4"], "every page is read, and no more");
            assert_eq!(
                env.store.romm_reconciliation(folder.id).unwrap(),
                Some(reconciled)
            );
            assert!(
                romm.seen().iter().all(|r| r.method == "GET"),
                "nothing is written to RomM"
            );
        }

        /// Discs in the ES-DE layout: a disc of several files in `<game>.cue/` and a Multi-disc Game in
        /// `<release>.m3u/`, its `.m3u` beside its discs. RomM sees each folder as one ROM, and each file in it is
        /// compared on its own.
        #[test]
        fn a_game_of_several_files_is_compared_file_by_file() {
            let orbit_cue = "FILE \"Orbit (USA) (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n";
            let saga_cue = |n: u8| {
                format!(
                    "FILE \"Saga (USA) (Disc {n}) (Track 1).bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n"
                )
            };
            let (saga1, saga2) = (saga_cue(1), saga_cue(2));
            let (orbit_bin, saga1_bin, saga2_bin) =
                (noise(1, 2352 * 4), noise(2, 2352 * 4), noise(3, 2352 * 4));
            let env = Env::of(
                "Redump",
                vec![
                    (
                        "Orbit (USA)",
                        vec![
                            rom("Orbit (USA).cue", orbit_cue.as_bytes()),
                            rom("Orbit (USA) (Track 1).bin", &orbit_bin),
                        ],
                    ),
                    (
                        "Saga (USA) (Disc 1)",
                        vec![
                            rom("Saga (USA) (Disc 1).cue", saga1.as_bytes()),
                            rom("Saga (USA) (Disc 1) (Track 1).bin", &saga1_bin),
                        ],
                    ),
                    (
                        "Saga (USA) (Disc 2)",
                        vec![
                            rom("Saga (USA) (Disc 2).cue", saga2.as_bytes()),
                            rom("Saga (USA) (Disc 2) (Track 1).bin", &saga2_bin),
                        ],
                    ),
                ],
                Some(SystemStorage {
                    format: Some(StorageFormat::LooseFolder),
                    layout: Some(DiscLayout::EsDe),
                    ..Default::default()
                }),
            );
            let folder = env.folder("psx");
            env.build_of(
                &folder,
                &["Orbit (USA)", "Saga (USA) (Disc 1)", "Saga (USA) (Disc 2)"],
            );
            let m3u = std::fs::read(env.dir("psx").join("Saga (USA).m3u/Saga (USA).m3u")).unwrap();
            let romm = library(
                "psx",
                vec![
                    rom_of(
                        1,
                        "psx",
                        "Orbit (USA).cue",
                        Some("Orbit"),
                        vec![
                            file(
                                "psx",
                                "Orbit (USA).cue/Orbit (USA) (Track 1).bin",
                                "0000000000000000000000000000000000000000",
                            ),
                            file(
                                "psx",
                                "Orbit (USA).cue/Orbit (USA).cue",
                                &sha1(orbit_cue.as_bytes()),
                            ),
                        ],
                    ),
                    rom_of(
                        2,
                        "psx",
                        "Saga (USA).m3u",
                        Some("Saga"),
                        vec![
                            file(
                                "psx",
                                "Saga (USA).m3u/Saga (USA) (Disc 1) (Track 1).bin",
                                &sha1(&saga1_bin),
                            ),
                            file(
                                "psx",
                                "Saga (USA).m3u/Saga (USA) (Disc 1).cue",
                                &sha1(saga1.as_bytes()),
                            ),
                            file(
                                "psx",
                                "Saga (USA).m3u/Saga (USA) (Disc 2) (Track 1).bin",
                                &sha1(&saga2_bin),
                            ),
                            file("psx", "Saga (USA).m3u/Saga (USA).m3u", &sha1(&m3u)),
                        ],
                    ),
                ],
            );
            env.romm(true, &romm.url, "play");

            let reconciled = reconcile(&env, &folder, 50).unwrap();

            assert_eq!(reconciled.error, None);
            assert_eq!(
                flags(&reconciled),
                [
                    (
                        "Orbit (USA).cue/Orbit (USA) (Track 1).bin",
                        vec![RommFlag::HashMismatch],
                        Some(1)
                    ),
                    (
                        "Saga (USA).m3u/Saga (USA) (Disc 2).cue",
                        vec![RommFlag::HashMismatch],
                        Some(2)
                    ),
                ]
            );
            assert_eq!(
                reconciled.counts,
                RommReconcileCounts {
                    files: 7,
                    clean: 5,
                    unidentified: 0,
                    title_mismatch: 0,
                    hash_mismatch: 2
                }
            );
            assert!(
                reconciled.rows[1]
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains("RomM doesn't list"),
                "{:?}",
                reconciled.rows[1]
            );
        }

        /// RomM hashes each member of a zip whole, so a ROM built with its Header agrees with RomM when RomM's hash is
        /// of the member with its Header, not of the DAT's headerless dump.
        #[test]
        fn a_rom_built_with_its_header_in_a_zip_is_compared_whole() {
            let mut ines = b"NES\x1a".to_vec();
            ines.resize(16, 0x01);
            let (zelda, mario) = (noise(1, 64), noise(2, 64));
            let env = Env::headered(
                "No-Intro",
                vec![
                    ("Zelda (USA)", vec![rom("Zelda (USA).nes", &zelda)]),
                    ("Mario (USA)", vec![rom("Mario (USA).nes", &mario)]),
                ],
                None,
                Some(("No-Intro_NES.xml", &ines)),
            );
            let folder = env.folder("famicom");
            env.build_of(&folder, &["Zelda (USA)", "Mario (USA)"]);
            let whole = [ines.clone(), zelda].concat();
            let romm = library(
                "famicom",
                vec![
                    rom_of(
                        1,
                        "famicom",
                        "Zelda (USA).zip",
                        Some("Zelda"),
                        vec![zip(
                            "famicom",
                            "Zelda (USA).zip",
                            &[("Zelda (USA).nes", &whole)],
                        )],
                    ),
                    rom_of(
                        2,
                        "famicom",
                        "Mario (USA).zip",
                        Some("Mario"),
                        vec![zip(
                            "famicom",
                            "Mario (USA).zip",
                            &[("Mario (USA).nes", &mario)],
                        )],
                    ),
                ],
            );
            env.romm(true, &romm.url, "play");

            let reconciled = reconcile(&env, &folder, 50).unwrap();

            assert_eq!(
                flags(&reconciled),
                [("Mario (USA).zip", vec![RommFlag::HashMismatch], Some(2))],
                "{reconciled:?}"
            );
            assert_eq!((reconciled.counts.files, reconciled.counts.clean), (2, 1));
        }

        #[test]
        fn only_the_latest_reconciliation_of_a_folder_is_kept_and_one_that_fails_says_why() {
            let env = atari();
            let folder = env.folder("a26");
            env.build_of(&folder, &ATARI);
            let romm = library("a26", vec![]);
            env.romm(true, &romm.url, "play");
            let first = reconcile(&env, &folder, 50).unwrap();
            assert_eq!(first.counts.hash_mismatch, 5);

            let elsewhere = library("nes", vec![]);
            env.romm(true, &elsewhere.url, "play");
            let failed = reconcile(&env, &folder, 50).unwrap();

            let error = failed.error.clone().expect("RomM has no platform a26");
            assert!(error.contains("a26"), "{error}");
            assert!(!error.contains("rmm_good"), "{error}");
            assert_eq!(
                (failed.counts, failed.rows.len()),
                (RommReconcileCounts::default(), 0)
            );
            assert_eq!(
                env.store.romm_reconciliation(folder.id).unwrap(),
                Some(failed)
            );
        }

        /// A platform RomM pages through more slowly than the scan window ends with a comparison that says so,
        /// instead of reading pages for ever.
        #[test]
        fn a_platform_too_slow_to_list_within_the_window_ends_saying_so() {
            let env = atari();
            let folder = env.folder("a26");
            env.build_of(&folder, &ATARI);
            let roms = (1..=20)
                .map(|id| rom_of(id, "a26", &format!("Game {id}.zip"), Some("Game"), vec![]))
                .collect();
            // The first page outlasts the window, which starts before it is asked for, so the window is over when
            // the next page is due.
            let window = ScanTiming {
                window: Duration::from_millis(50),
                ..FAST
            };
            let romm = library_then("a26", roms, move |r| {
                if r.path.starts_with("/api/roms?") {
                    std::thread::sleep(window.window);
                }
            });
            env.romm(true, &romm.url, "play");

            let ended = Store::reconcile_romm(
                &Arc::downgrade(&env.store),
                &env.config,
                folder.id,
                None,
                1,
                &window,
                &ScanStop::default(),
            )
            .unwrap()
            .unwrap();

            let error = ended.error.expect("the comparison ran out of time");
            assert!(error.contains("didn't list"), "{error}");
            let pages = romm
                .seen()
                .iter()
                .filter(|r| r.path.starts_with("/api/roms?"))
                .count();
            assert_eq!(pages, 1, "it checks the window between pages");
            assert!(error.contains("(1 read)"), "{error}");
        }

        /// [`library`], holding its answer to the first request for a page of ROMs until the test has done what it
        /// does then.
        fn held_library(fs_slug: &str, roms: Vec<Value>) -> (StubRomm, Hold) {
            let (reached, wait_reached) = std::sync::mpsc::channel::<()>();
            let (release, released) = std::sync::mpsc::channel::<()>();
            let (reached, released) = (Mutex::new(reached), Mutex::new(released));
            let once = AtomicUsize::new(0);
            let romm = library_then(fs_slug, roms, move |r| {
                if r.path.starts_with("/api/roms?") && once.fetch_add(1, Ordering::SeqCst) == 0 {
                    reached.lock().unwrap().send(()).unwrap();
                    released.lock().unwrap().recv().unwrap();
                }
            });
            (romm, Hold(wait_reached, release))
        }

        /// RomM turned off, or its address or root changed, while its last page of ROMs is read: the comparison is
        /// of RomM as you no longer have it, so it is discarded and the one before stays the latest.
        #[test]
        fn a_comparison_whose_romm_changed_while_it_read_the_last_page_is_discarded() {
            let env = atari();
            let folder = env.folder("a26");
            env.build_of(&folder, &ATARI);
            let elsewhere = library("a26", vec![]);
            type Change<'a> = Box<dyn Fn(&Env, &str) + 'a>;
            let changes: [(&str, Change); 3] = [
                ("off", Box::new(|env, url| env.romm(false, url, "play"))),
                (
                    "address",
                    Box::new(|env, _| env.romm(true, &elsewhere.url, "play")),
                ),
                ("root", Box::new(|env, url| env.romm(true, url, "sets"))),
            ];
            for (name, change) in changes {
                let (romm, hold) = held_library("a26", vec![]);
                let before = library("a26", vec![]);
                env.romm(true, &before.url, "play");
                let latest = reconcile(&env, &folder, 50).unwrap();
                env.romm(true, &romm.url, "play");

                let reconciled = std::thread::scope(|s| {
                    let reconciling = s.spawn(|| reconcile(&env, &folder, 50));
                    hold.when_reached(|| change(&env, &romm.url));
                    reconciling.join().unwrap()
                });

                assert_eq!(reconciled, None, "{name}");
                assert_eq!(
                    env.store.romm_reconciliation(folder.id).unwrap(),
                    Some(latest),
                    "{name}"
                );
            }
            assert!(elsewhere.seen().is_empty());
        }

        /// A comparison's ROM ids are RomM's at the address it read, for the platform the folder was then: once
        /// either changes, a rescan is refused rather than sent where the id may name another ROM.
        #[test]
        fn a_rescan_is_refused_once_romm_s_address_or_the_folder_s_platform_changed() {
            let env = atari();
            let folder = env.folder("a26");
            env.build_of(&folder, &ATARI);
            let roms = vec![rom_of(
                2,
                "a26",
                "Combat (USA).zip",
                None,
                vec![zip(
                    "a26",
                    "Combat (USA).zip",
                    &[("Combat (USA).a26", b"combat")],
                )],
            )];
            let (romm, elsewhere) = (library("a26", roms.clone()), library("a26", roms));
            env.romm(true, &romm.url, "play");
            reconcile(&env, &folder, 50).unwrap();

            for (name, url, root) in [
                ("address", &elsewhere.url, "play"),
                ("platform", &romm.url, "sets"),
            ] {
                env.romm(true, url, root);
                let refused = env
                    .store
                    .request_romm_rescan(&env.config, folder.id, 2)
                    .unwrap_err();
                let title_select_store::Error::Refused(why) = &refused else {
                    panic!("{name}: {refused}");
                };
                assert!(why.contains("Compare again"), "{name}: {why}");
            }
            env.romm(true, &romm.url, "play");
            env.store
                .request_romm_rescan(&env.config, folder.id, 2)
                .unwrap();

            let posts = |r: &StubRomm| r.seen().iter().filter(|s| s.method == "POST").count();
            assert_eq!((posts(&romm), posts(&elsewhere)), (1, 0));
        }

        #[test]
        fn nothing_is_reconciled_while_romm_is_off_or_for_a_folder_romm_does_not_scan() {
            let env = atari();
            let romm = library("a26", vec![]);
            let folder = env.folder("a26");
            env.build_of(&folder, &ATARI);
            env.romm(false, &romm.url, "play");
            assert_eq!(reconcile(&env, &folder, 50), None);

            env.romm(true, &romm.url, "play");
            let nested = env.folder("arcade/a26");
            env.build_of(&nested, &ATARI);
            assert_eq!(reconcile(&env, &nested, 50), None);

            assert!(romm.seen().is_empty(), "{:?}", romm.seen());
            assert_eq!(env.store.romm_reconciliation(folder.id).unwrap(), None);
        }

        #[test]
        fn a_rescan_asks_romm_to_scan_one_rom_of_the_latest_reconciliation_again() {
            let env = atari();
            let folder = env.folder("a26");
            env.build_of(&folder, &ATARI);
            let romm = library(
                "a26",
                vec![
                    rom_of(
                        2,
                        "a26",
                        "Combat (USA).zip",
                        None,
                        vec![zip(
                            "a26",
                            "Combat (USA).zip",
                            &[("Combat (USA).a26", b"combat")],
                        )],
                    ),
                    rom_of(
                        4,
                        "a26",
                        "Breakout (USA).zip",
                        Some("Breakout"),
                        vec![zip(
                            "a26",
                            "Breakout (USA).zip",
                            &[("Breakout (USA).a26", b"x")],
                        )],
                    ),
                ],
            );
            env.romm(true, &romm.url, "play");
            reconcile(&env, &folder, 50).unwrap();

            let unmatched = env
                .store
                .request_romm_rescan(&env.config, folder.id, 2)
                .unwrap();
            let update = env
                .store
                .request_romm_rescan(&env.config, folder.id, 4)
                .unwrap();

            assert_eq!(
                (unmatched.scan_type.as_str(), unmatched.task_id.as_str()),
                ("unmatched", "t9")
            );
            assert_eq!(update.scan_type, "update");
            let posted: Vec<Value> = romm
                .seen()
                .into_iter()
                .filter(|r| r.path == "/api/tasks/scan")
                .map(|r| serde_json::from_str(&r.body).unwrap())
                .collect();
            assert_eq!(
                posted,
                [
                    json!({"type": "unmatched", "roms_ids": [2]}),
                    json!({"type": "update", "roms_ids": [4]})
                ]
            );

            let not_flagged = env
                .store
                .request_romm_rescan(&env.config, folder.id, 99)
                .unwrap_err();
            assert!(
                matches!(not_flagged, title_select_store::Error::Refused(_)),
                "{not_flagged}"
            );
            env.romm(false, &romm.url, "play");
            let off = env
                .store
                .request_romm_rescan(&env.config, folder.id, 2)
                .unwrap_err();
            assert!(
                matches!(off, title_select_store::Error::Refused(_)),
                "{off}"
            );
            assert_eq!(romm.seen().iter().filter(|r| r.method == "POST").count(), 2);
        }
    }
}
