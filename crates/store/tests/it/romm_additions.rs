//! RomM's "Additions" collection (#142): kept in step with the Additions built into mapped Library Folders after each
//! scan a Build requests, recreated when it disappears, deleted when RomM is turned off, and never touched by a token
//! lacking `collections.write`. Against a stub RomM on a local port that keeps collections as RomM 5.4.1 does
//! (research 0136); no test sends a request to a real RomM.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::common::*;
use sha1::Digest;
use title_select_api::additions::{EditAddition, NewAddition};
use title_select_api::build::{AddLibraryFolder, LibraryFolder};
use title_select_api::ingest::{IngestAction, PlanIngest};
use title_select_api::romm::{RommAdditions, RommAdditionsOutcome, RommScanOutcome, RommUpdate};
use title_select_api::selection::{
    CoverageDrift, Rule, Selection, SelectionGroup, SelectionPins, SelectionProfile,
    SelectionSummary, SystemSettings, Winner,
};
use title_select_api::settings::{IngestCadence, SecretUpdate, SettingsUpdate};
use title_select_api::{AuditEvidence, Job, JobKind, JobState};
use title_select_store::config::Config;
use title_select_store::romm::{Released, ScanStop, ScanTiming};
use title_select_store::{Store, Workers};

const SET: &str = "Test - System";
const SYSTEM: &str = "No-Intro/Test - System";
const PROFILE: &str = "Mine";
const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};
const FAST: ScanTiming = ScanTiming {
    poll: Duration::from_millis(10),
    backoff: Duration::from_millis(10),
    backoff_max: Duration::from_millis(40),
    window: Duration::from_secs(5),
    request: Duration::from_secs(5),
};
const ALL: [&str; 5] = [
    "roms.read",
    "platforms.read",
    "collections.read",
    "collections.write",
    "tasks.run",
];

/// A request the stub saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    authorization: Option<String>,
    content_type: Option<String>,
    body: String,
}

/// The RomM user the token `rmm_good` signs in as.
const ME: i64 = 3;

/// A static collection as RomM keeps it.
#[derive(Debug, Clone)]
struct Collection {
    name: String,
    public: bool,
    rom_ids: BTreeSet<i64>,
    owner: i64,
}

impl Collection {
    fn new(name: &str, public: bool, owner: i64) -> Collection {
        Collection {
            name: name.into(),
            public,
            rom_ids: BTreeSet::new(),
            owner,
        }
    }
}

/// Holds each by-hash request while armed, saying when one waits.
#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    signal: Condvar,
}

impl Gate {
    fn arm(&self) {
        self.state.lock().unwrap().0 = true;
    }

    fn release(&self) {
        self.state.lock().unwrap().0 = false;
        self.signal.notify_all();
    }

    /// Waits until a request waits at the gate.
    fn wait_for_one(&self) {
        let state = self.state.lock().unwrap();
        let _held = self
            .signal
            .wait_timeout_while(state, Duration::from_secs(10), |(_, waiting)| !*waiting)
            .unwrap();
        assert!(_held.0.1, "a request reached the gate");
    }

    fn pass(&self) {
        let mut state = self.state.lock().unwrap();
        if state.0 {
            state.1 = true;
            self.signal.notify_all();
            state = self.signal.wait_while(state, |(armed, _)| *armed).unwrap();
        }
        state.1 = false;
    }
}

/// What the stub RomM holds: the ROMs it has indexed, by SHA-1, and its collections.
#[derive(Debug, Default)]
struct Library {
    indexed: BTreeMap<String, i64>,
    collections: BTreeMap<i64, Collection>,
    next: i64,
    /// Whether deleting a collection fails with HTTP 500.
    failing_deletes: bool,
}

/// A stub RomM 5.4.1 that scans at once, finds ROMs it has indexed by hash, and keeps static collections, answering
/// only the bearer of `rmm_good` and only with a scope it holds.
struct StubRomm {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    library: Arc<Mutex<Library>>,
    gate: Arc<Gate>,
}

impl StubRomm {
    fn start(scopes: &[&str]) -> StubRomm {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let library = Arc::new(Mutex::new(Library {
            next: 7,
            ..Library::default()
        }));
        let scopes: BTreeSet<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
        let gate: Arc<Gate> = Arc::default();
        let (s, l, g) = (seen.clone(), library.clone(), gate.clone());
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
                let (mut authorization, mut content_type, mut length) = (None, None, 0usize);
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        let value = value.trim().to_owned();
                        if name.eq_ignore_ascii_case("authorization") {
                            authorization = Some(value);
                        } else if name.eq_ignore_ascii_case("content-type") {
                            content_type = Some(value);
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
                    content_type,
                    body: String::from_utf8_lossy(&body).into_owned(),
                };
                s.lock().unwrap().push(request.clone());
                if request.path.starts_with("/api/roms/by-hash") {
                    g.pass();
                }
                let (status, body) = answer(&request, &scopes, &mut l.lock().unwrap());
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        StubRomm {
            url,
            seen,
            library,
            gate,
        }
    }

    /// RomM indexes the file whose payload is `payload` as the ROM `id`.
    fn index(&self, payload: &[u8], id: i64) {
        let sha1 = hex(&sha1::Sha1::digest(payload));
        self.library.lock().unwrap().indexed.insert(sha1, id);
    }

    fn collections(&self) -> BTreeMap<i64, Collection> {
        self.library.lock().unwrap().collections.clone()
    }

    /// Takes the requests seen so far.
    fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn collection_json(id: i64, c: &Collection) -> String {
    serde_json::json!({
        "id": id, "name": c.name, "description": "", "rom_ids": c.rom_ids, "is_public": c.public,
        "is_favorite": false, "is_virtual": false, "is_smart": false, "user_id": c.owner,
        "owner_username": if c.owner == ME { "svc" } else { "someone" },
        "url_cover": "", "path_cover_small": null, "path_cover_large": null, "path_covers_small": [],
        "path_covers_large": [], "created_at": "2026-11-15T00:00:00Z", "updated_at": "2026-11-15T00:00:00Z",
        "rom_count": c.rom_ids.len(),
    })
    .to_string()
}

/// The value of the multipart form field `field` in `body`.
fn form_field(body: &str, field: &str) -> Option<String> {
    let marker = format!("name=\"{field}\"\r\n\r\n");
    let start = body.find(&marker)? + marker.len();
    let end = body[start..].find("\r\n--")? + start;
    Some(body[start..end].to_owned())
}

/// How RomM 5.4.1 answers `r`, for `endpoints/collections.py`, `endpoints/roms/__init__.py` (`/by-hash`) and the
/// scan's task.
fn answer(r: &Seen, scopes: &BTreeSet<String>, lib: &mut Library) -> (u16, String) {
    if (r.method.as_str(), r.path.as_str()) == ("GET", "/api/heartbeat") {
        return (200, r#"{"SYSTEM": {"VERSION": "5.4.1"}}"#.into());
    }
    if r.authorization.as_deref() != Some("Bearer rmm_good") {
        return (401, r#"{"detail": "Not authenticated"}"#.into());
    }
    let not_found = |what: &str| (404, format!(r#"{{"detail": "{what} not found"}}"#));
    let (path, query) = r.path.split_once('?').unwrap_or((&r.path, ""));
    let parts: Vec<&str> = path.trim_start_matches("/api/").split('/').collect();
    let scope = match (r.method.as_str(), &parts[..]) {
        ("POST", ["tasks", "scan"]) | ("GET", ["tasks", _]) => "tasks.run",
        ("GET", ["roms", "by-hash"]) | ("GET", ["roms"]) => "roms.read",
        ("GET", ["collections", ..]) => "collections.read",
        (_, ["collections", ..]) => "collections.write",
        _ => return not_found("route"),
    };
    if !scopes.contains(scope) {
        return (403, r#"{"detail": "Forbidden"}"#.into());
    }
    let rom_ids = || -> Vec<i64> {
        serde_json::from_str::<serde_json::Value>(&r.body).unwrap()["rom_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect()
    };
    let id = |s: &str| s.parse::<i64>().unwrap();
    let forbidden = (403, r#"{"detail": "Forbidden"}"#.to_owned());
    // Only the owner changes a collection, and only the owner sees a private one.
    if let ["collections", n, ..] = &parts[..]
        && let Ok(n) = n.parse::<i64>()
        && let Some(c) = lib.collections.get(&n)
        && c.owner != ME
        && (r.method != "GET" || !c.public)
    {
        return forbidden;
    }
    match (r.method.as_str(), &parts[..]) {
        ("POST", ["tasks", "scan"]) => (202, r#"{"task_id": "t1", "status": "queued"}"#.into()),
        ("GET", ["tasks", "t1"]) => (200, r#"{"task_id": "t1", "status": "finished"}"#.into()),
        ("GET", ["roms", "by-hash"]) => {
            let sha1 = query.strip_prefix("sha1_hash=").expect("found by SHA-1");
            match lib.indexed.get(sha1) {
                Some(rom) => (200, format!(r#"{{"id": {rom}, "sha1_hash": "{sha1}"}}"#)),
                None => not_found("ROM"),
            }
        }
        ("GET", ["collections", "identifiers"]) => (
            200,
            serde_json::to_string(&lib.collections.keys().collect::<Vec<_>>()).unwrap(),
        ),
        ("GET", ["collections"]) => (
            200,
            format!(
                "[{}]",
                lib.collections
                    .iter()
                    .filter(|(_, c)| c.owner == ME || c.public)
                    .map(|(id, c)| collection_json(*id, c))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ),
        ("GET", ["collections", n]) => match lib.collections.get(&id(n)) {
            Some(c) => (200, collection_json(id(n), c)),
            None => not_found("Collection"),
        },
        ("POST", ["collections"]) => {
            assert!(
                r.content_type
                    .as_deref()
                    .is_some_and(|t| t.starts_with("multipart/form-data; boundary=")),
                "RomM reads a new collection's fields as a form: {r:?}"
            );
            let name = form_field(&r.body, "name").unwrap_or_default();
            if lib
                .collections
                .values()
                .any(|c| c.name == name && c.owner == ME)
            {
                return (
                    409,
                    format!(r#"{{"detail": "Collection with name '{name}' already exists"}}"#),
                );
            }
            let new = lib.next;
            lib.next += 1;
            let c = Collection::new(&name, query.split('&').any(|q| q == "is_public=true"), ME);
            let json = collection_json(new, &c);
            lib.collections.insert(new, c);
            (200, json)
        }
        ("POST", ["collections", n, "roms"]) => match lib.collections.get_mut(&id(n)) {
            Some(c) => {
                c.rom_ids.extend(rom_ids());
                (200, collection_json(id(n), c))
            }
            None => not_found("Collection"),
        },
        ("DELETE", ["collections", n, "roms"]) => match lib.collections.get_mut(&id(n)) {
            Some(c) => {
                for rom in rom_ids() {
                    c.rom_ids.remove(&rom);
                }
                (200, collection_json(id(n), c))
            }
            None => not_found("Collection"),
        },
        ("PUT", ["collections", n, "visibility"]) => match lib.collections.get_mut(&id(n)) {
            Some(c) => {
                let body: serde_json::Value = serde_json::from_str(&r.body).unwrap();
                c.public = body["is_public"].as_bool().expect("is_public, embedded");
                (200, collection_json(id(n), c))
            }
            None => not_found("Collection"),
        },
        ("DELETE", ["collections", _]) if lib.failing_deletes => {
            (500, r#"{"detail": "Internal Server Error"}"#.into())
        }
        ("DELETE", ["collections", n]) => match lib.collections.remove(&id(n)) {
            Some(_) => (200, "null".into()),
            None => not_found("Collection"),
        },
        _ => not_found("route"),
    }
}

/// A store with the Source Set `Test - System`, holding Pong; the Ingest Folder `in`; the writable Root `play`, which
/// RomM mounts as its library, holding the Library Folder `atari`; and the Root `additions`. Builds read the system's
/// Additions Set.
struct Env {
    tmp: tempfile::TempDir,
    store: Arc<Store>,
    config: Config,
    sha256: String,
    folder: LibraryFolder,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        for dir in [
            "sets/Test - System",
            "in",
            "play/atari",
            "additions",
            "dats",
        ] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let path = |p: &str| tmp.path().join(p).display().to_string();
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n\n[roots.in]\npath = {:?}\nwritable = true\nkind = \"ingest\"\n\n\
             [roots.play]\npath = {:?}\nwritable = true\n\n[roots.additions]\npath = {:?}\nwritable = true\n",
            path("sets"),
            path("in"),
            path("play"),
            path("additions"),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        store
            .update_settings(
                &config,
                &SettingsUpdate {
                    ingest_folders: Some(
                        [(
                            "in".to_owned(),
                            IngestCadence {
                                settle_secs: 0,
                                scan_every_minutes: None,
                            },
                        )]
                        .into(),
                    ),
                    ..Default::default()
                },
                "Operator",
            )
            .unwrap();
        let dat = write_dat_version(
            &tmp.path().join("dats"),
            "test.dat",
            "1",
            &[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])],
        );
        let (revision, _) = store.import_dat(&dat, None).unwrap();
        store.add_source_set(&config, "sets", SET, SYSTEM).unwrap();
        let mut profile = SelectionProfile::default();
        profile.systems.insert(
            SET.into(),
            SystemSettings {
                additions: true,
                ..SystemSettings::default()
            },
        );
        store
            .save_profile(PROFILE, &profile, None, "Operator", &[], WORKERS)
            .unwrap();
        store.register_ingest_folder(&config, "in").unwrap();
        write_file(
            &tmp.path().join("sets").join(SET).join("Pong (USA).a26"),
            b"pong",
        );
        store
            .audit_source_set(&config, SET, None, AuditEvidence::Full)
            .unwrap();
        let folder = store
            .add_library_folder(
                &config,
                &AddLibraryFolder {
                    root: "play".into(),
                    path: Some("atari".into()),
                    template: None,
                    source_set: SET.into(),
                    profile: PROFILE.into(),
                },
            )
            .unwrap();
        Env {
            tmp,
            store: Arc::new(store),
            config,
            sha256: revision.sha256,
            folder,
        }
    }

    fn romm(&self, url: &str) {
        self.settings(true, url, SecretUpdate::Set("rmm_good".into()));
    }

    fn settings(&self, enabled: bool, url: &str, token: SecretUpdate) {
        self.store
            .update_settings(&self.config, &romm_update(enabled, url, token), "Operator")
            .unwrap();
    }

    fn ingest_dir(&self) -> PathBuf {
        self.tmp.path().join("in")
    }

    /// Approves the Ingest Folder file `file`, holding `payload`, as the Addition `name`, and publishes it.
    fn add(&self, file: &str, name: &str, payload: &[u8]) {
        write_file(&self.ingest_dir().join(file), payload);
        for _ in 0..2 {
            let job = self.store.enqueue_ingest_scan("in", "Operator").unwrap();
            self.store.claim_job().unwrap().unwrap();
            let ended = self
                .store
                .run_ingest_scan_job(&self.config, job.id)
                .unwrap();
            assert_eq!(ended.state, JobState::Succeeded, "{ended:?}");
        }
        let item = self
            .store
            .ingest_items("in")
            .unwrap()
            .into_iter()
            .filter(|i| i.name == file)
            .max_by_key(|i| i.id)
            .unwrap()
            .id;
        let plan = self
            .store
            .plan_ingest(
                &self.config,
                "in",
                &PlanIngest {
                    action: IngestAction::Addition,
                    items: vec![item],
                    additions: BTreeMap::from([(
                        item,
                        NewAddition {
                            system: SYSTEM.into(),
                            name: name.into(),
                            parent: None,
                        },
                    )]),
                },
            )
            .unwrap();
        self.store
            .approve_ingest_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.settle();
    }

    /// The id of the Addition `name`.
    fn addition(&self, name: &str) -> i64 {
        self.store
            .additions_sets()
            .unwrap()
            .into_iter()
            .flat_map(|s| s.additions)
            .find(|a| a.name == name)
            .unwrap()
            .id
    }

    fn settle(&self) {
        while let Some(job) = self.store.claim_job().unwrap() {
            assert_eq!(job.kind, JobKind::Additions, "{job:?}");
            let job = self.store.run_additions_job(&self.config, job.id).unwrap();
            assert_eq!(job.state, JobState::Succeeded, "{job:?}");
        }
    }

    /// Builds Pong and every active Addition into the Library Folder, then follows the scan it requests and keeps
    /// the collection in step. Returns the Job as it then is.
    fn build(&self) -> Job {
        let job = self.build_only();
        self.scan(job);
        self.sync(job);
        self.store.job(job).unwrap()
    }

    /// Builds Pong and every active Addition into the Library Folder, and returns the Job's id.
    fn build_only(&self) -> i64 {
        self.store
            .enqueue_selection(SET, None, PROFILE, None, "Operator", WORKERS)
            .unwrap();
        let job = self.store.claim_job().unwrap().unwrap();
        let selection = self
            .store
            .record_selection(job.id, &pong(&self.sha256))
            .unwrap()
            .selection
            .unwrap();
        let plan = self
            .store
            .plan_build(&self.config, self.folder.id, selection)
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
        self.settle();
        job.id
    }

    /// Follows the scan the Build Job `job` requests to its end.
    fn scan(&self, job: i64) {
        let weak = Arc::downgrade(&self.store);
        let scanned = Store::request_romm_scan(
            &weak,
            &self.config,
            job,
            &FAST,
            &ScanStop::default(),
            &mut |_| {},
        )
        .unwrap()
        .expect("the build owes a scan");
        assert_eq!(
            scanned.romm_scan.as_ref().unwrap().outcome,
            RommScanOutcome::Finished
        );
    }

    /// Keeps the collection in step once the Build Job `job`'s scan finished.
    fn sync(&self, job: i64) {
        let weak = Arc::downgrade(&self.store);
        let mut told = vec![];
        let synced = Store::sync_romm_additions(
            &weak,
            &self.config,
            job,
            &FAST,
            &ScanStop::default(),
            &mut |j| told.push(j.clone()),
        )
        .unwrap();
        assert_eq!(
            synced.as_ref(),
            told.last(),
            "the end is the last step told"
        );
    }

    /// Deletes the collection turning RomM off left, if any.
    fn delete_released(&self) -> Released {
        Store::delete_released_romm_collection(&Arc::downgrade(&self.store)).unwrap()
    }

    fn files(&self) -> BTreeSet<String> {
        std::fs::read_dir(self.tmp.path().join("play/atari"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.'))
            .collect()
    }
}

fn romm_update(enabled: bool, url: &str, token: SecretUpdate) -> SettingsUpdate {
    SettingsUpdate {
        romm: Some(RommUpdate {
            enabled,
            base_url: Some(url.into()),
            token,
            root: Some("play".into()),
        }),
        ..SettingsUpdate::default()
    }
}

fn pong(sha256: &str) -> Selection {
    Selection {
        pins: SelectionPins::default(),
        dat_sha256: sha256.into(),
        system: SET.into(),
        system_settings: false,
        summary: SelectionSummary::default(),
        coverage_drift: CoverageDrift::default(),
        groups: vec![SelectionGroup {
            group: "pong (usa)".into(),
            unresolved: false,
            winners: vec![Winner {
                title: "Pong (USA)".into(),
                rule: Rule {
                    stage: "Parent selection".into(),
                    reference: None,
                    message: "m".into(),
                },
                unresolved: false,
            }],
            alternatives: vec![],
            exclusions: vec![],
            choice: None,
            drift: false,
        }],
        stale_choices: BTreeMap::new(),
    }
}

fn additions_of(job: &Job) -> RommAdditions {
    job.romm_scan
        .as_ref()
        .and_then(|s| s.additions.clone())
        .unwrap_or_else(|| panic!("the collection was kept in step: {job:?}"))
}

/// The collection requests among `seen`, as `METHOD path`, in order.
fn collection_calls(seen: &[Seen]) -> Vec<String> {
    seen.iter()
        .filter(|r| r.path.starts_with("/api/collections"))
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

fn rom_ids(seen: &[Seen], method: &str, path: &str) -> Vec<i64> {
    let r = seen
        .iter()
        .find(|r| r.method == method && r.path == path)
        .unwrap_or_else(|| panic!("{method} {path} was sent: {seen:#?}"));
    let body: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    let mut ids: Vec<i64> = serde_json::from_value(body["rom_ids"].clone()).unwrap();
    ids.sort();
    ids
}

#[test]
fn the_additions_collection_follows_each_build_is_recreated_when_gone_and_deleted_when_romm_is_turned_off()
 {
    let romm = StubRomm::start(&ALL);
    let env = Env::new();
    env.romm(&romm.url);
    env.add("hack.a26", "My Hack", b"hack");
    env.add("other.a26", "Other Hack", b"other");
    romm.index(b"hack", 101);

    // The first build creates the public collection "Additions" and adds what RomM has indexed; the Addition RomM
    // hasn't indexed is listed as unresolved, a warning on a job that succeeded.
    let job = env.build();
    assert_eq!(
        env.files(),
        BTreeSet::from(["Pong (USA).zip", "My Hack.zip", "Other Hack.zip"].map(String::from))
    );
    assert_eq!(job.state, JobState::Succeeded);
    let synced = additions_of(&job);
    assert_eq!(synced.outcome, RommAdditionsOutcome::Synced, "{synced:?}");
    assert_eq!(synced.collection_id, Some(7));
    assert_eq!((synced.members, synced.added, synced.removed), (1, 1, 0));
    assert_eq!(synced.unresolved, ["Other Hack"]);
    assert!(synced.is_warning());
    let collections = romm.collections();
    assert_eq!(collections.len(), 1, "{collections:?}");
    let created = &collections[&7];
    assert_eq!(created.name, "Additions");
    assert!(created.public);
    assert_eq!(created.rom_ids, BTreeSet::from([101]));
    let seen = romm.take();
    assert!(
        seen.iter()
            .any(|r| r.method == "GET" && r.path.starts_with("/api/roms/by-hash?sha1_hash=")),
        "{seen:#?}"
    );
    assert_eq!(rom_ids(&seen, "POST", "/api/collections/7/roms"), [101]);
    assert!(
        !seen.iter().any(|r| r.method == "DELETE"),
        "nothing to remove: {seen:#?}"
    );

    // Once RomM indexes the other and the first is retired, the next build adds one and removes the other.
    romm.index(b"other", 102);
    env.store.retire_addition(env.addition("My Hack")).unwrap();
    env.settle();
    let job = env.build();
    assert!(!env.files().contains("My Hack.zip"));
    let synced = additions_of(&job);
    assert_eq!(synced.outcome, RommAdditionsOutcome::Synced, "{synced:?}");
    assert_eq!((synced.members, synced.added, synced.removed), (1, 1, 1));
    assert!(
        synced.unresolved.is_empty() && !synced.is_warning(),
        "{synced:?}"
    );
    assert_eq!(romm.collections()[&7].rom_ids, BTreeSet::from([102]));
    let seen = romm.take();
    assert_eq!(rom_ids(&seen, "POST", "/api/collections/7/roms"), [102]);
    assert_eq!(rom_ids(&seen, "DELETE", "/api/collections/7/roms"), [101]);
    assert!(
        !seen
            .iter()
            .any(|r| r.method == "POST" && r.path.starts_with("/api/collections?")),
        "the collection is found by its id, not created again: {seen:#?}"
    );

    // Deleted in RomM, it is created again at the next build that changes the folder.
    romm.library.lock().unwrap().collections.clear();
    env.store
        .edit_addition(
            env.addition("Other Hack"),
            &EditAddition {
                name: Some("Renamed Hack".into()),
                parent: None,
            },
        )
        .unwrap();
    let job = env.build();
    let synced = additions_of(&job);
    assert_eq!(synced.outcome, RommAdditionsOutcome::Synced, "{synced:?}");
    assert_eq!(synced.collection_id, Some(8), "{synced:?}");
    let collections = romm.collections();
    assert_eq!(collections.keys().collect::<Vec<_>>(), [&8]);
    assert_eq!(collections[&8].rom_ids, BTreeSet::from([102]));
    romm.take();

    // Turning RomM off, even clearing its token at once, deletes the collection with the token saved before.
    let (_, pending) = env
        .store
        .update_settings_and_release(
            &env.config,
            &romm_update(false, &romm.url, SecretUpdate::Clear),
            "Operator",
            |_| {},
        )
        .unwrap();
    assert!(pending, "turning RomM off leaves its collection to delete");
    assert_eq!(env.delete_released(), Released::Deleted(8));
    assert!(romm.collections().is_empty());
    let seen = romm.take();
    assert_eq!(collection_calls(&seen), ["DELETE /api/collections/8"]);
    assert_eq!(seen[0].authorization.as_deref(), Some("Bearer rmm_good"));
    assert_eq!(env.delete_released(), Released::Nothing, "deleted once");

    // Turned on again, a build creates a new one.
    env.romm(&romm.url);
    env.add("third.a26", "Third Hack", b"third");
    let job = env.build();
    assert_eq!(additions_of(&job).collection_id, Some(9));
}

#[test]
fn turning_romm_off_with_no_collection_or_changing_nothing_leaves_none_to_delete() {
    let romm = StubRomm::start(&ALL);
    let env = Env::new();
    env.romm(&romm.url);
    let (_, left) = env
        .store
        .update_settings_and_release(
            &env.config,
            &romm_update(true, &romm.url, SecretUpdate::Keep),
            "Operator",
            |_| {},
        )
        .unwrap();
    assert!(!left);
    let (_, left) = env
        .store
        .update_settings_and_release(
            &env.config,
            &romm_update(false, &romm.url, SecretUpdate::Keep),
            "Operator",
            |_| {},
        )
        .unwrap();
    assert!(!left, "no collection was ever made");
    assert_eq!(env.delete_released(), Released::Nothing);
    assert!(romm.take().is_empty());
}

#[test]
fn without_collections_write_no_collection_is_touched_and_the_warning_names_the_scope() {
    let romm = StubRomm::start(&[
        "roms.read",
        "platforms.read",
        "collections.read",
        "tasks.run",
    ]);
    let env = Env::new();
    env.romm(&romm.url);
    env.add("hack.a26", "My Hack", b"hack");
    romm.index(b"hack", 101);

    let job = env.build();
    assert_eq!(job.state, JobState::Succeeded);
    let synced = additions_of(&job);
    assert_eq!(synced.outcome, RommAdditionsOutcome::Error, "{synced:?}");
    assert!(synced.is_warning());
    let detail = synced.detail.unwrap();
    assert!(detail.contains("collections.write"), "{detail}");
    assert!(romm.collections().is_empty());
    let seen = romm.take();
    assert_eq!(
        collection_calls(&seen),
        ["POST /api/collections/0/roms"],
        "only the scope check, which adds nothing to a collection RomM never makes"
    );
    assert!(
        !seen.iter().any(|r| r.path.starts_with("/api/roms/by-hash")),
        "{seen:#?}"
    );
}

#[test]
fn a_sync_reads_the_built_additions_once_it_has_its_turn() {
    let romm = StubRomm::start(&ALL);
    let env = Env::new();
    env.romm(&romm.url);
    env.add("hack.a26", "My Hack", b"hack");
    romm.index(b"hack", 101);
    let first = env.build_only();
    env.scan(first);
    env.add("other.a26", "Other Hack", b"other");
    romm.index(b"other", 102);
    let second = env.build_only();
    env.scan(second);

    // The first build's sync waits on RomM while the second's waits its turn; meanwhile My Hack is retired.
    romm.gate.arm();
    std::thread::scope(|scope| {
        scope.spawn(|| env.sync(first));
        romm.gate.wait_for_one();
        scope.spawn(|| env.sync(second));
        std::thread::sleep(Duration::from_millis(200));
        env.store.retire_addition(env.addition("My Hack")).unwrap();
        env.settle();
        env.build_only();
        romm.gate.release();
    });

    assert_eq!(
        romm.collections()[&7].rom_ids,
        BTreeSet::from([102]),
        "the later sync holds what is built when it runs, not when it was asked"
    );
}

#[test]
fn your_own_untracked_additions_collection_is_adopted_and_kept_public() {
    let romm = StubRomm::start(&ALL);
    {
        let mut lib = romm.library.lock().unwrap();
        lib.collections
            .insert(1, Collection::new("Additions", true, 4));
        lib.collections
            .insert(2, Collection::new("Additions", false, ME));
    }
    let env = Env::new();
    env.romm(&romm.url);
    env.add("hack.a26", "My Hack", b"hack");
    romm.index(b"hack", 101);

    // RomM refuses a second "Additions" of yours; yours is adopted, not another user's public one, and shared.
    let synced = additions_of(&env.build());
    assert_eq!(synced.outcome, RommAdditionsOutcome::Synced, "{synced:?}");
    assert_eq!(synced.collection_id, Some(2));
    let collections = romm.collections();
    assert!(collections[&2].public);
    assert_eq!(collections[&2].rom_ids, BTreeSet::from([101]));
    assert!(
        collections[&1].rom_ids.is_empty(),
        "another user's is never touched"
    );
    let seen = romm.take();
    assert!(
        seen.iter().any(|r| r.method == "PUT"
            && r.path == "/api/collections/2/visibility"
            && r.body.contains("\"is_public\":true")),
        "{seen:#?}"
    );

    // Made private in RomM, it is shared again at the next sync.
    romm.library
        .lock()
        .unwrap()
        .collections
        .get_mut(&2)
        .unwrap()
        .public = false;
    env.add("other.a26", "Other Hack", b"other");
    let synced = additions_of(&env.build());
    assert_eq!(synced.collection_id, Some(2));
    assert!(romm.collections()[&2].public);
}

#[test]
fn a_deletion_romm_refuses_is_kept_and_retried_until_it_succeeds() {
    let romm = StubRomm::start(&ALL);
    let env = Env::new();
    env.romm(&romm.url);
    env.add("hack.a26", "My Hack", b"hack");
    romm.index(b"hack", 101);
    assert_eq!(additions_of(&env.build()).collection_id, Some(7));

    // RomM refuses the deletion: it stays pending, the token it needs kept out of sight.
    romm.library.lock().unwrap().failing_deletes = true;
    let (_, pending) = env
        .store
        .update_settings_and_release(
            &env.config,
            &romm_update(false, &romm.url, SecretUpdate::Clear),
            "Operator",
            |_| {},
        )
        .unwrap();
    assert!(pending);
    let refused = env.delete_released();
    assert!(
        matches!(
            refused,
            Released::Failed {
                collection_id: 7,
                ..
            }
        ),
        "{refused:?}"
    );
    assert!(!format!("{refused:?}").contains("rmm_good"));
    assert!(romm.collections().contains_key(&7));

    // Turned on again, the next sync deletes it first, then makes a new one.
    romm.library.lock().unwrap().failing_deletes = false;
    env.romm(&romm.url);
    romm.take();
    env.add("other.a26", "Other Hack", b"other");
    assert_eq!(additions_of(&env.build()).collection_id, Some(8));
    let seen = romm.take();
    assert_eq!(
        collection_calls(&seen).first().map(String::as_str),
        Some("DELETE /api/collections/7"),
        "{seen:#?}"
    );
    assert!(!romm.collections().contains_key(&7));
    assert_eq!(env.delete_released(), Released::Nothing);

    // One already gone in RomM counts as deleted.
    romm.library.lock().unwrap().failing_deletes = true;
    env.settings(false, &romm.url, SecretUpdate::Keep);
    assert!(matches!(env.delete_released(), Released::Failed { .. }));
    {
        let mut lib = romm.library.lock().unwrap();
        lib.collections.clear();
        lib.failing_deletes = false;
    }
    assert_eq!(env.delete_released(), Released::Deleted(8));
    assert_eq!(env.delete_released(), Released::Nothing);
}

#[test]
fn a_pending_deletion_never_deletes_the_collection_adopted_again() {
    let romm = StubRomm::start(&ALL);
    let env = Env::new();
    env.romm(&romm.url);
    env.add("hack.a26", "My Hack", b"hack");
    romm.index(b"hack", 101);
    assert_eq!(additions_of(&env.build()).collection_id, Some(7));

    // Turned off, RomM refuses the deletion; turned on again at the same address, it refuses again, so the next
    // sync adopts the same collection.
    romm.library.lock().unwrap().failing_deletes = true;
    env.settings(false, &romm.url, SecretUpdate::Keep);
    assert!(matches!(env.delete_released(), Released::Failed { .. }));
    env.romm(&romm.url);
    env.add("other.a26", "Other Hack", b"other");
    romm.index(b"other", 102);
    let synced = additions_of(&env.build());
    assert_eq!(synced.outcome, RommAdditionsOutcome::Synced, "{synced:?}");
    assert_eq!(synced.collection_id, Some(7));

    // Once RomM would delete it, nothing is left to delete: the collection in use stays.
    romm.library.lock().unwrap().failing_deletes = false;
    romm.take();
    assert_eq!(env.delete_released(), Released::Nothing);
    assert_eq!(romm.collections()[&7].rom_ids, BTreeSet::from([101, 102]));
    assert!(collection_calls(&romm.take()).is_empty());
}
