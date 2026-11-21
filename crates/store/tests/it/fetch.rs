//! Fetching newer DAT Revisions for Following Source Sets (ADR 0012), against a fake DAT Provider on a local port.
//! No test sends a request anywhere else.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use title_select_api::fetch::{
    FetchArtifact, FetchOutcome, FetchProvider, FetchReport, FetchTrigger, SUPPORT_SYSTEM,
};
use title_select_api::settings::{FetchSettings, SettingsUpdate};
use title_select_api::support::{SupportFileKind, SupportFileOrigin};
use title_select_api::{AuditEvidence, CuePackOrigin, SourceSetStale};
use title_select_engine::cap::Clock;
use title_select_store::config::Config;
use title_select_store::fetch::{
    Allowed, Endpoints, FetchClock, FetchError, Fetcher, Http, MIN_GAP,
};
use title_select_store::{Error, Store};

/// What the fake provider answers a path with.
#[derive(Clone)]
enum Reply {
    Body(Vec<u8>),
    Status(u16),
    Redirect(String),
}

/// A request the fake provider saw.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    user_agent: Option<String>,
}

/// A fake DAT Provider: an HTTP/1.1 server on a local port that answers each path as told and records each request.
struct FakeProvider {
    url: String,
    replies: Arc<Mutex<HashMap<String, Reply>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeProvider {
    fn start() -> FakeProvider {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let replies: Arc<Mutex<HashMap<String, Reply>>> = Arc::default();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let (r, s) = (replies.clone(), seen.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                let mut user_agent = None;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':')
                        && name.eq_ignore_ascii_case("user-agent")
                    {
                        user_agent = Some(value.trim().to_owned());
                    }
                }
                s.lock().unwrap().push(Seen {
                    path: path.clone(),
                    user_agent,
                });
                let reply = r.lock().unwrap().get(&path).cloned();
                let (status, extra, body) = match reply {
                    Some(Reply::Body(body)) => (200, String::new(), body),
                    Some(Reply::Status(status)) => (status, String::new(), Vec::new()),
                    Some(Reply::Redirect(to)) => (302, format!("Location: {to}\r\n"), Vec::new()),
                    None => (404, String::new(), Vec::new()),
                };
                let head = format!(
                    "HTTP/1.1 {status} X\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        FakeProvider { url, replies, seen }
    }

    fn reply(&self, path: &str, reply: Reply) {
        self.replies.lock().unwrap().insert(path.to_owned(), reply);
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

#[test]
fn no_request_can_address_a_no_intro_host() {
    let http = Http::new();
    // Even an allowlist naming No-Intro's hosts can't let a request through.
    let allowed = Allowed::hosts(&[
        "datomatic.no-intro.org",
        "no-intro.org",
        "forum.no-intro.org",
    ]);
    for url in [
        "https://datomatic.no-intro.org/index.php?page=download",
        "http://no-intro.org/",
        "https://NO-INTRO.ORG./x",
        "https://forum.No-Intro.org:443/",
        "http://user@datomatic.no-intro.org/",
    ] {
        match http.get(url, &allowed, None, 1 << 20) {
            Err(FetchError::Forbidden(_)) => {}
            other => panic!("{url}: {other:?}"),
        }
    }
}

#[test]
fn a_redirect_to_a_no_intro_host_is_refused_before_it_is_followed() {
    let fake = FakeProvider::start();
    fake.reply(
        "/datfile/psx/",
        Reply::Redirect("https://datomatic.no-intro.org/index.php".into()),
    );
    match Http::new().get(
        &format!("{}/datfile/psx/", fake.url),
        &Allowed::origin(&fake.url),
        None,
        1 << 20,
    ) {
        Err(FetchError::Forbidden(_)) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(fake.seen().len(), 1, "only the first request was sent");
}

#[test]
fn a_request_names_title_select_and_follows_an_allowed_redirect() {
    let fake = FakeProvider::start();
    fake.reply("/a", Reply::Redirect(format!("{}/b", fake.url)));
    fake.reply("/b", Reply::Body(b"payload".to_vec()));
    let got = Http::new()
        .get(
            &format!("{}/a", fake.url),
            &Allowed::origin(&fake.url),
            None,
            1 << 20,
        )
        .unwrap();
    assert_eq!(got.bytes, b"payload");
    let seen = fake.seen();
    assert_eq!(seen.len(), 2);
    let agent = seen[0].user_agent.clone().unwrap();
    assert!(agent.starts_with("title-select/"), "{agent}");
}

#[test]
fn a_response_larger_than_its_limit_or_not_ok_fails() {
    let fake = FakeProvider::start();
    fake.reply("/big", Reply::Body(vec![0; 2048]));
    fake.reply("/gone", Reply::Status(503));
    let http = Http::new();
    let allowed = Allowed::origin(&fake.url);
    assert!(matches!(
        http.get(&format!("{}/big", fake.url), &allowed, None, 1024),
        Err(FetchError::TooLarge { .. })
    ));
    assert!(matches!(
        http.get(&format!("{}/gone", fake.url), &allowed, None, 1024),
        Err(FetchError::Status { status: 503, .. })
    ));
}

#[test]
fn each_provider_may_address_only_its_own_hosts_by_name_on_their_default_ports() {
    let endpoints = Endpoints::official();
    let redump = endpoints.allowed(FetchProvider::Redump);
    for url in [
        "http://redump.org/datfile/psx/",
        "https://www.redump.org/cues/psx/",
        "http://Redump.org.:80/sbi/psx/",
    ] {
        assert_eq!(redump.refusal(url), None, "{url}");
    }
    for url in [
        "http://93.184.216.34/datfile/psx/",
        "http://[2001:db8::1]/datfile/psx/",
        "http://redump.org:8080/datfile/psx/",
        "https://redump.org:80/datfile/psx/",
        "http://redump.org@example.com/datfile/psx/",
        "http://user:pass@redump.org/datfile/psx/",
        "http://redump.org.example.com/",
        "http://mirror.redump.org/",
        "https://api.github.com/repos/mamedev/mame/releases/latest",
        "ftp://redump.org/",
    ] {
        assert!(redump.refusal(url).is_some(), "{url}");
    }
    let mame = endpoints.allowed(FetchProvider::Mame);
    for url in [
        "https://api.github.com/repos/mamedev/mame/releases/latest",
        "https://github.com/mamedev/mame/releases/download/mame0270/mame0270lx.zip",
        "https://release-assets.githubusercontent.com/github-production-release-asset/1",
        "https://objects.githubusercontent.com/github-production-release-asset-2e65be/1",
        "https://raw.githubusercontent.com/mamedev/mame/mame0270/hash/nes.xml",
        "https://api.github.com/repos/AntoPISA/MAME_SupportFiles/commits/main",
        "https://raw.githubusercontent.com/AntoPISA/MAME_SupportFiles/ad2049f2b06181c0e2dabda710f3b712cf97d36d/catver.ini/catver.ini",
    ] {
        assert_eq!(mame.refusal(url), None, "{url}");
    }
    for url in [
        "http://redump.org/datfile/psx/",
        "https://gist.githubusercontent.com/x",
        "https://github.com:444/mamedev/mame/",
        "https://140.82.112.3/mamedev/mame/",
        "https://github.com@example.com/",
    ] {
        assert!(mame.refusal(url).is_some(), "{url}");
    }
}

#[test]
fn a_redirect_off_its_providers_hosts_is_refused_before_it_is_followed() {
    let fake = FakeProvider::start();
    let elsewhere = FakeProvider::start();
    let host = fake.url.trim_start_matches("http://");
    fake.reply("/ip", Reply::Redirect(format!("{}/x", elsewhere.url)));
    fake.reply("/off", Reply::Redirect("http://example.com/x".into()));
    fake.reply(
        "/userinfo",
        Reply::Redirect(format!("http://{host}@example.com/x")),
    );
    let allowed = Allowed::origin(&fake.url);
    for path in ["/ip", "/off", "/userinfo"] {
        match Http::new().get(&format!("{}{path}", fake.url), &allowed, None, 1 << 20) {
            Err(FetchError::Forbidden(_)) => {}
            other => panic!("{path}: {other:?}"),
        }
    }
    assert_eq!(
        fake.seen().len(),
        3,
        "only the first request of each was sent"
    );
    assert!(elsewhere.seen().is_empty());
}

/// As [`dat_xml`], with a header that names Redump, as a DAT downloaded from Redump has.
fn redump_dat_xml(name: &str, version: &str) -> Vec<u8> {
    String::from_utf8(dat_xml(name, version))
        .unwrap()
        .replace("</version>", "</version><homepage>redump.org</homepage>")
        .into_bytes()
}

/// A Logiqx DAT named `name` at `version`, with one Game, whose header names no provider.
fn dat_xml(name: &str, version: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0"?>
<datafile>
  <header><name>{name}</name><version>{version}</version></header>
  <game name="Game (USA)"><rom name="Game (USA).bin" size="4" crc="d87f7e0c"/></game>
</datafile>
"#
    )
    .into_bytes()
}

/// A ZIP of `members`, in memory.
fn zip_of(members: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write as _;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, payload) in members {
        zip.start_file(*name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(payload).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

/// A fetcher's fake clock: time moves only as the fetcher waits, from the wall time it started at, and it records each
/// wait. A fetcher waits before each request, so the waits are the gaps between them.
struct Paced {
    started: jiff::Timestamp,
    now: Mutex<u64>,
    waits: Mutex<Vec<Duration>>,
}

impl Paced {
    fn start() -> Arc<Paced> {
        Arc::new(Paced {
            started: jiff::Timestamp::now(),
            now: Mutex::new(0),
            waits: Mutex::default(),
        })
    }

    fn waits(&self) -> Vec<Duration> {
        self.waits.lock().unwrap().clone()
    }
}

impl Clock for Paced {
    fn now(&self) -> u64 {
        *self.now.lock().unwrap()
    }

    fn sleep(&self, d: Duration) {
        *self.now.lock().unwrap() += u64::try_from(d.as_nanos()).unwrap();
        self.waits.lock().unwrap().push(d);
    }
}

impl FetchClock for Paced {
    fn wall(&self) -> jiff::Timestamp {
        self.started + Duration::from_nanos(self.now())
    }
}

/// A store with the Source Set `PSX`, bound to `Redump/Sony - PlayStation` at version 1, audited and Following, and a
/// fake provider standing in for every official endpoint.
struct Env {
    _tmp: tempfile::TempDir,
    store: Store,
    config: Config,
    fake: FakeProvider,
    pace: Arc<Paced>,
    fetcher: Fetcher,
}

const REDUMP: FetchSettings = FetchSettings {
    redump: true,
    mame: false,
};
const MAME: FetchSettings = FetchSettings {
    redump: false,
    mame: true,
};

impl Env {
    fn new(fetch: FetchSettings) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("sets/PSX")).unwrap();
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n",
            tmp.path().join("sets").display().to_string(),
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        store
            .update_settings(
                &config,
                &SettingsUpdate {
                    fetch: Some(fetch),
                    ..SettingsUpdate::default()
                },
                "Operator",
            )
            .unwrap();
        let fake = FakeProvider::start();
        let pace = Paced::start();
        let fetcher = Fetcher::paced(Endpoints::at(&fake.url), pace.clone());
        let env = Env {
            _tmp: tmp,
            store,
            config,
            fake,
            pace,
            fetcher,
        };
        env.follow("PSX", "Sony - PlayStation");
        env
    }

    /// Imports version 1 of `Redump/<dat>` and registers, audits and follows the Source Set `name` bound to it.
    fn follow(&self, name: &str, dat: &str) {
        std::fs::create_dir_all(self._tmp.path().join("sets").join(name)).unwrap();
        self.store
            .import_dat_bytes(&redump_dat_xml(dat, "1"), "v1.dat", None)
            .unwrap();
        self.store
            .add_source_set(&self.config, "sets", name, &format!("Redump/{dat}"))
            .unwrap();
        self.store
            .audit_source_set(&self.config, name, None, AuditEvidence::Incremental)
            .unwrap();
        self.store.set_follow(name, true).unwrap();
    }

    fn fetch(&self, provider: FetchProvider, trigger: FetchTrigger) -> Result<FetchReport, Error> {
        self.store
            .fetch(&self.fetcher, provider, trigger, "Operator")
    }

    fn serve_psx(&self, version: &str) {
        let dat = zip_of(&[(
            "Sony - PlayStation (2026).dat",
            &dat_xml("Sony - PlayStation", version),
        )]);
        self.fake.reply("/datfile/psx/", Reply::Body(dat));
        self.serve_psx_packs(version);
    }

    /// Serves a cue pack and an SBI pack for `psx` whose contents differ with `version`.
    fn serve_psx_packs(&self, version: &str) {
        let cue = format!("FILE \"Game (USA).bin\" BINARY {version}");
        self.fake.reply(
            "/cues/psx/",
            Reply::Body(zip_of(&[("Game (USA).cue", cue.as_bytes())])),
        );
        let sbi = format!("SBI\0{version}");
        self.fake.reply(
            "/sbi/psx/",
            Reply::Body(zip_of(&[("Game (USA).sbi", sbi.as_bytes())])),
        );
    }

    /// The SHA-256s of the SBI packs whose files the store keeps.
    fn sbi_packs(&self) -> Vec<String> {
        self.kept("sbi_packs")
    }

    /// The SHA-256s of the cue packs whose files the store keeps.
    fn cue_packs(&self) -> Vec<String> {
        self.kept("cue_packs")
    }

    fn kept(&self, packs: &str) -> Vec<String> {
        let mut found: Vec<String> = std::fs::read_dir(self.store.dir().join(packs))
            .map(|d| {
                d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        found.sort();
        found
    }

    fn audits(&self) -> i64 {
        let db = rusqlite::Connection::open(self.store.dir().join("title-select.sqlite")).unwrap();
        db.query_row("SELECT count(*) FROM audits", [], |r| r.get(0))
            .unwrap()
    }

    /// Moves every recorded request back by `hours`.
    fn backdate(&self, hours: i64) {
        let db = rusqlite::Connection::open(self.store.dir().join("title-select.sqlite")).unwrap();
        let mut rows = db.prepare("SELECT id, started_at FROM fetches").unwrap();
        let rows: Vec<(i64, String)> = rows
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for (id, at) in rows {
            let at: jiff::Timestamp = at.parse().unwrap();
            let at = at - jiff::SignedDuration::from_hours(hours);
            db.execute(
                "UPDATE fetches SET started_at = ?2, finished_at = ?2 WHERE id = ?1",
                rusqlite::params![id, at.to_string()],
            )
            .unwrap();
        }
    }
}

fn outcomes(report: &FetchReport) -> Vec<(String, FetchArtifact, Option<FetchOutcome>)> {
    report
        .attempts
        .iter()
        .map(|a| (a.system.clone(), a.artifact, a.outcome))
        .collect()
}

#[test]
fn a_redump_fetch_brings_a_newer_dat_and_its_packs_a_second_apart_and_starts_no_audit() {
    let env = Env::new(REDUMP);
    env.serve_psx("2");
    let (jobs, audits) = (env.store.jobs(100).unwrap(), env.audits());

    let report = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(report.stopped, None);
    let added = Some(FetchOutcome::Added);
    assert_eq!(
        outcomes(&report),
        [
            ("psx".into(), FetchArtifact::Dat, added),
            ("psx".into(), FetchArtifact::Cues, added),
            ("psx".into(), FetchArtifact::Sbi, added),
        ]
    );
    let fetched = env
        .store
        .dat_revisions()
        .unwrap()
        .into_iter()
        .find(|r| r.version == "2")
        .unwrap();
    assert_eq!(
        (fetched.dat.as_str(), fetched.provider.as_deref()),
        ("Redump/Sony - PlayStation", Some("Redump")),
        "its header names no provider, so it takes the fetch's"
    );
    let seen = env.fake.seen();
    let paths: Vec<_> = seen.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(paths, ["/datfile/psx/", "/cues/psx/", "/sbi/psx/"]);
    assert_eq!(env.pace.waits(), [MIN_GAP; 3], "a second before each");
    for s in &seen {
        assert!(
            s.user_agent
                .as_deref()
                .unwrap()
                .starts_with("title-select/")
        );
    }

    let dat = &report.attempts[0];
    assert_eq!(dat.version.as_deref(), Some("2"));
    let stale = env.store.followed_revisions("PSX").unwrap();
    assert_eq!(stale.len(), 1, "the following source set is stale");
    assert_eq!(Some(&stale[0].sha256), dat.dat_revision.as_ref());
    let cues = env.store.cue_packs().unwrap();
    assert_eq!(cues.len(), 1, "the cue pack is in the cue pack store");
    assert_eq!(cues[0].origin, CuePackOrigin::Fetch);
    assert_eq!(cues[0].dat, "Redump/Sony - PlayStation");
    assert_eq!(cues[0].cues, 1);
    assert_eq!(report.attempts[1].pack.as_ref(), Some(&cues[0].sha256));
    assert!(
        cues[0]
            .source_url
            .as_deref()
            .unwrap()
            .ends_with("/cues/psx/")
    );
    assert_eq!(
        env.sbi_packs(),
        [report.attempts[2].pack.clone().unwrap()],
        "the SBI pack is kept"
    );
    assert_eq!(env.store.jobs(100).unwrap(), jobs, "fetching queues no job");
    assert_eq!(env.audits(), audits, "fetching starts no audit");
}

impl Env {
    /// Records a request to `provider`'s system `other` that finished `ago` before now, as an earlier Service would.
    fn record_request(&self, provider: &str, ago: Duration) {
        let db = rusqlite::Connection::open(self.store.dir().join("title-select.sqlite")).unwrap();
        let at = self.pace.wall() - jiff::SignedDuration::try_from(ago).unwrap();
        db.execute(
            "INSERT INTO fetches (provider, system, artifact, trigger, actor, url, started_at, finished_at, outcome)
             VALUES (?1, 'other', 'dat', 'manual', 'Operator', 'x', ?2, ?2, 'unchanged')",
            rusqlite::params![provider, at.to_string()],
        )
        .unwrap();
    }

    /// How long a fetch from Redump waited before its first request.
    fn first_request_after(&self) -> Duration {
        self.fetch(FetchProvider::Redump, FetchTrigger::Manual)
            .unwrap();
        assert!(!self.fake.seen().is_empty());
        self.pace.waits()[0]
    }
}

#[test]
fn a_new_fetcher_waits_a_second_after_the_last_request_any_service_recorded() {
    let env = Env::new(REDUMP);
    env.serve_psx("2");
    env.record_request("redump", Duration::from_millis(300));
    assert_eq!(env.first_request_after(), Duration::from_millis(700));
}

#[test]
fn a_new_fetcher_with_no_request_recorded_waits_a_second_first() {
    let env = Env::new(REDUMP);
    env.serve_psx("2");
    assert_eq!(env.first_request_after(), MIN_GAP);
}

#[test]
fn a_new_fetcher_whose_last_recorded_request_is_long_past_sends_at_once() {
    let env = Env::new(REDUMP);
    env.serve_psx("2");
    env.record_request("redump", Duration::from_secs(10));
    env.record_request("mame", Duration::ZERO);
    assert_eq!(env.first_request_after(), Duration::ZERO);
}

#[test]
fn each_system_is_fetched_at_most_once_a_day_even_by_hand() {
    let env = Env::new(REDUMP);
    env.serve_psx("2");
    env.fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();

    match env.fetch(FetchProvider::Redump, FetchTrigger::Manual) {
        Err(Error::Refused(why)) => assert!(why.contains("once a day"), "{why}"),
        other => panic!("{other:?}"),
    }
    let scheduled = env
        .fetch(FetchProvider::Redump, FetchTrigger::Schedule)
        .unwrap();
    assert_eq!(scheduled.attempts, []);
    assert_eq!(env.fake.seen().len(), 3, "nothing more was requested");

    env.backdate(25);
    let again = env
        .fetch(FetchProvider::Redump, FetchTrigger::Schedule)
        .unwrap();
    let unchanged = Some(FetchOutcome::Unchanged);
    assert_eq!(
        outcomes(&again),
        [
            ("psx".into(), FetchArtifact::Dat, unchanged),
            ("psx".into(), FetchArtifact::Cues, unchanged),
            ("psx".into(), FetchArtifact::Sbi, unchanged),
        ]
    );
}

#[test]
fn a_failed_request_stops_the_fetch_and_keeps_the_last_good_revision() {
    let env = Env::new(REDUMP);
    env.follow("PS2", "Sony - PlayStation 2");
    env.serve_psx("2");
    env.fake.reply("/datfile/psx/", Reply::Status(503));

    let report = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(
        outcomes(&report),
        [("psx".into(), FetchArtifact::Dat, Some(FetchOutcome::Failed))]
    );
    assert!(report.stopped.is_some());
    assert!(report.attempts[0].error.as_deref().unwrap().contains("503"));
    assert_eq!(env.fake.seen().len(), 1, "nothing after the first error");
    assert_eq!(env.store.followed_revisions("PSX").unwrap(), []);
    assert_eq!(
        env.store.dat_revisions().unwrap().len(),
        2,
        "version 1 of each DAT"
    );

    let status = env.store.fetch_status(&env.fetcher).unwrap();
    let redump = &status.providers[0];
    assert_eq!(redump.provider, FetchProvider::Redump);
    let psx = redump
        .systems
        .iter()
        .find(|s| s.system.as_deref() == Some("psx"))
        .unwrap();
    assert_eq!(psx.source_sets, ["PSX"]);
    let dat = &psx.artifacts[0];
    assert_eq!(dat.artifact, FetchArtifact::Dat);
    assert_eq!(
        dat.last_attempt.as_ref().unwrap().outcome,
        Some(FetchOutcome::Failed)
    );
    assert_eq!(dat.last_success, None);
    assert!(dat.next_at.is_some(), "a failure also waits a day");
}

#[test]
fn a_redirect_off_its_providers_hosts_fails_the_step_and_stops_the_fetch() {
    let env = Env::new(REDUMP);
    let elsewhere = FakeProvider::start();
    env.serve_psx("2");
    env.fake.reply(
        "/datfile/psx/",
        Reply::Redirect(format!("{}/psx.zip", elsewhere.url)),
    );

    let report = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(
        outcomes(&report),
        [("psx".into(), FetchArtifact::Dat, Some(FetchOutcome::Failed))]
    );
    assert!(report.stopped.is_some());
    assert!(elsewhere.seen().is_empty());
    assert_eq!(env.fake.seen().len(), 1);
}

#[test]
fn a_dat_for_another_system_fails_the_fetch() {
    let env = Env::new(REDUMP);
    let other = zip_of(&[("x.dat", &dat_xml("Sega - Saturn", "2"))]);
    env.fake.reply("/datfile/psx/", Reply::Body(other));

    let report = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(report.attempts[0].outcome, Some(FetchOutcome::Failed));
    assert_eq!(env.store.dat_revisions().unwrap().len(), 1);
}

#[test]
fn a_fetch_of_bytes_already_imported_under_another_dat_fails_naming_both_and_moves_nothing() {
    let env = Env::new(REDUMP);
    let bytes = dat_xml("Sony - PlayStation", "2");
    let (manual, _) = env.store.import_dat_bytes(&bytes, "psx.dat", None).unwrap();
    assert_eq!(manual.dat, "unknown-logiqx/Sony - PlayStation");
    env.fake.reply(
        "/datfile/psx/",
        Reply::Body(zip_of(&[("Sony - PlayStation (2026).dat", &bytes)])),
    );

    let report = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    let attempt = &report.attempts[0];
    assert_eq!(attempt.outcome, Some(FetchOutcome::Failed));
    let error = attempt.error.as_deref().unwrap();
    assert!(
        error.contains("unknown-logiqx/Sony - PlayStation")
            && error.contains("Redump/Sony - PlayStation"),
        "{error}"
    );
    let revisions = env.store.dat_revisions().unwrap();
    let kept = revisions
        .iter()
        .find(|r| r.sha256 == manual.sha256)
        .unwrap();
    assert_eq!(kept.dat, "unknown-logiqx/Sony - PlayStation", "not moved");
}

#[test]
fn fetching_is_off_until_you_turn_it_on_and_only_for_following_systems() {
    let env = Env::new(FetchSettings::default());
    env.serve_psx("2");
    match env.fetch(FetchProvider::Redump, FetchTrigger::Manual) {
        Err(Error::Refused(why)) => assert!(why.contains("turn on fetching from Redump"), "{why}"),
        other => panic!("{other:?}"),
    }
    let status = env.store.fetch_status(&env.fetcher).unwrap();
    assert!(status.providers.iter().all(|p| !p.enabled));

    let env = Env::new(REDUMP);
    env.serve_psx("2");
    env.store.set_follow("PSX", false).unwrap();
    match env.fetch(FetchProvider::Redump, FetchTrigger::Manual) {
        Err(Error::Refused(why)) => assert!(why.contains("follow"), "{why}"),
        other => panic!("{other:?}"),
    }
    env.store.set_follow("PSX", true).unwrap();
    env.store.unregister_source_set(&env.config, "PSX").unwrap();
    assert!(
        env.fetch(FetchProvider::Redump, FetchTrigger::Manual)
            .is_err()
    );
    let status = env.store.fetch_status(&env.fetcher).unwrap();
    assert_eq!(
        status.providers[0].systems,
        [],
        "an unregistered set follows nothing"
    );
    assert_eq!(env.fake.seen().len(), 0, "nothing was requested");
}

#[test]
fn a_system_is_fetched_while_any_registered_source_set_follows_it() {
    let env = Env::new(REDUMP);
    std::fs::create_dir_all(env._tmp.path().join("sets/PSX2")).unwrap();
    env.store
        .add_source_set(&env.config, "sets", "PSX2", "Redump/Sony - PlayStation")
        .unwrap();
    env.store.set_follow("PSX2", true).unwrap();
    env.store.unregister_source_set(&env.config, "PSX").unwrap();
    env.serve_psx("2");

    let report = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(report.attempts.len(), 3, "{report:?}");
}

#[test]
fn a_system_whose_dat_is_followed_under_two_spellings_of_its_provider_is_requested_once() {
    let env = Env::new(REDUMP);
    std::fs::create_dir_all(env._tmp.path().join("sets/PSX2")).unwrap();
    let mut other = redump_dat_xml("Sony - PlayStation", "1");
    other.push(b'\n');
    env.store.import_dat_bytes(&other, "v1.dat", None).unwrap();
    env.store
        .add_source_set(&env.config, "sets", "PSX2", "redump/Sony - PlayStation")
        .unwrap();
    env.store
        .audit_source_set(&env.config, "PSX2", None, AuditEvidence::Incremental)
        .unwrap();
    env.store.set_follow("PSX2", true).unwrap();
    env.serve_psx("2");

    let report = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(report.stopped, None, "{report:?}");
    let mut paths: Vec<String> = env.fake.seen().into_iter().map(|s| s.path).collect();
    paths.sort();
    assert_eq!(paths, ["/cues/psx/", "/datfile/psx/", "/sbi/psx/"]);
    assert_eq!(report.attempts.len(), 3);
    for set in ["PSX", "PSX2"] {
        assert!(
            matches!(&env.store.source_set(set).unwrap().stale[..], [SourceSetStale::NewerRevision { version, .. }] if version == "2"),
            "{set}"
        );
    }
}

#[test]
fn two_fetchers_at_once_send_each_request_once() {
    let env = Env::new(REDUMP);
    env.serve_psx("2");
    let other = Fetcher::paced(Endpoints::at(&env.fake.url), Paced::start());

    std::thread::scope(|scope| {
        for fetcher in [&env.fetcher, &other] {
            let env = &env;
            scope.spawn(move || {
                let _ = env.store.fetch(
                    fetcher,
                    FetchProvider::Redump,
                    FetchTrigger::Schedule,
                    "Operator",
                );
            });
        }
    });
    let mut paths: Vec<String> = env.fake.seen().into_iter().map(|s| s.path).collect();
    paths.sort();
    assert_eq!(paths, ["/cues/psx/", "/datfile/psx/", "/sbi/psx/"]);
}

#[test]
fn a_deleted_redump_revision_fetched_again_is_added_not_unchanged() {
    let env = Env::new(REDUMP);
    env.serve_psx("2");
    let first = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    let fetched = first.attempts[0].dat_revision.clone().unwrap();
    env.store.unregister_source_set(&env.config, "PSX").unwrap();
    env.store.delete_dat_revision(&fetched).unwrap();
    env.follow("PSX again", "Sony - PlayStation");

    env.backdate(25);
    let again = env
        .fetch(FetchProvider::Redump, FetchTrigger::Schedule)
        .unwrap();
    assert_eq!(again.attempts[0].artifact, FetchArtifact::Dat);
    assert_eq!(again.attempts[0].outcome, Some(FetchOutcome::Added));
    assert_eq!(
        again.attempts[0].dat_revision.as_deref(),
        Some(fetched.as_str())
    );
    assert!(
        env.store
            .dat_revisions()
            .unwrap()
            .iter()
            .any(|r| r.sha256 == fetched && r.blob_present)
    );
}

#[test]
fn only_what_redump_publishes_for_a_system_is_requested() {
    let env = Env::new(REDUMP);
    env.store.set_follow("PSX", false).unwrap();
    env.follow("PS2", "Sony - PlayStation 2");
    env.follow("GC", "Nintendo - GameCube");
    env.fake.reply(
        "/datfile/ps2/",
        Reply::Body(zip_of(&[("x.dat", &dat_xml("Sony - PlayStation 2", "2"))])),
    );
    env.fake.reply(
        "/cues/ps2/",
        Reply::Body(zip_of(&[("Game (USA).cue", b"FILE")])),
    );
    env.fake.reply(
        "/datfile/gc/",
        Reply::Body(zip_of(&[("x.dat", &dat_xml("Nintendo - GameCube", "2"))])),
    );

    let report = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(report.stopped, None, "{report:?}");
    let mut paths: Vec<String> = env.fake.seen().into_iter().map(|s| s.path).collect();
    paths.sort();
    assert_eq!(
        paths,
        ["/cues/ps2/", "/datfile/gc/", "/datfile/ps2/"],
        "no SBI pack for either, and no cue pack for the GameCube"
    );
}

#[test]
fn a_newer_pack_releases_the_last_ones_file_and_the_latest_is_kept() {
    let env = Env::new(REDUMP);
    env.serve_psx("2");
    let first = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    env.backdate(25);
    env.serve_psx_packs("3");
    let second = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    let added = Some(FetchOutcome::Added);
    assert_eq!(second.attempts[1].outcome, added, "{second:?}");
    assert_eq!(second.attempts[2].outcome, added, "{second:?}");

    let cues = env.store.cue_packs().unwrap();
    let kept: Vec<(String, bool)> = cues
        .iter()
        .map(|c| (c.sha256.clone(), c.blob_present))
        .collect();
    assert_eq!(
        kept,
        [
            (first.attempts[1].pack.clone().unwrap(), false),
            (second.attempts[1].pack.clone().unwrap(), true),
        ]
    );
    assert_eq!(env.sbi_packs(), [second.attempts[2].pack.clone().unwrap()]);
}

#[test]
fn the_sbi_pack_kept_is_the_one_the_latest_fetch_brought_even_if_it_came_before() {
    let env = Env::new(REDUMP);
    env.serve_psx("2");
    let a = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    let a = a.attempts[2].pack.clone().unwrap();
    env.backdate(25);
    env.serve_psx_packs("3");
    env.fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    env.backdate(25);
    env.serve_psx_packs("2");
    let again = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(
        again.attempts[2].pack.as_deref(),
        Some(a.as_str()),
        "{again:?}"
    );

    env.store.retain_sbi_packs().unwrap();
    assert_eq!(env.sbi_packs(), [a]);
}

#[test]
fn the_cue_pack_kept_is_the_one_the_latest_fetch_brought_even_if_it_came_before() {
    let env = Env::new(REDUMP);
    env.serve_psx("2");
    let a = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    let a = a.attempts[1].pack.clone().unwrap();
    env.backdate(25);
    env.serve_psx_packs("3");
    env.fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    env.backdate(25);
    env.serve_psx_packs("2");
    let again = env
        .fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(
        again.attempts[1].pack.as_deref(),
        Some(a.as_str()),
        "{again:?}"
    );

    env.store.retain_cue_packs().unwrap();
    assert_eq!(env.cue_packs(), [a]);
}

/// A MAME ListXML at build `build`.
fn listxml(build: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0"?>
<mame build="{build}">
  <machine name="pong"><rom name="pong.bin" size="4" crc="d87f7e0c"/></machine>
</mame>
"#
    )
    .into_bytes()
}

fn software_list(name: &str, rev: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0"?>
<softwarelist name="{name}" description="Test">
  <software name="game{rev}"><description>Game</description>
    <part name="cart" interface="cart"><dataarea name="rom" size="4">
      <rom name="game.bin" size="4" crc="d87f7e0c"/>
    </dataarea></part>
  </software>
</softwarelist>
"#
    )
    .into_bytes()
}

impl Env {
    fn serve_mame(&self, tag: &str) {
        let url = &self.fake.url;
        let asset = format!("{url}/mamedev/mame/releases/download/{tag}/{tag}lx.zip");
        let release = format!(
            r#"{{"tag_name":"{tag}","assets":[{{"name":"{tag}s.exe","browser_download_url":"{url}/x.exe"}},
               {{"name":"{tag}lx.zip","browser_download_url":"{asset}"}}]}}"#
        );
        self.fake.reply(
            "/repos/mamedev/mame/releases/latest",
            Reply::Body(release.into_bytes()),
        );
        self.fake.reply(
            &format!("/mamedev/mame/releases/download/{tag}/{tag}lx.zip"),
            Reply::Redirect(format!("{url}/objects/{tag}lx.zip")),
        );
        self.fake.reply(
            &format!("/objects/{tag}lx.zip"),
            Reply::Body(zip_of(&[(&format!("{tag}.xml"), &listxml(tag))])),
        );
        self.fake.reply(
            &format!("/mamedev/mame/{tag}/hash/nes.xml"),
            Reply::Body(software_list("nes", tag)),
        );
    }
}

#[test]
fn a_mame_fetch_finds_the_latest_release_and_brings_its_listxml_and_software_lists() {
    let env = Env::new(MAME);
    env.store
        .import_dat_bytes(&listxml("mame0001"), "mame.xml", None)
        .unwrap();
    std::fs::create_dir_all(env._tmp.path().join("sets/Arcade")).unwrap();
    env.store
        .add_source_set(&env.config, "sets", "Arcade", "MAME/MAME")
        .unwrap();
    env.store
        .audit_source_set(&env.config, "Arcade", None, AuditEvidence::Incremental)
        .unwrap();
    env.store.set_follow("Arcade", true).unwrap();
    env.store
        .import_dat_bytes(&software_list("nes", "0"), "nes.xml", None)
        .unwrap();
    std::fs::create_dir_all(env._tmp.path().join("sets/NES")).unwrap();
    env.store
        .add_source_set(&env.config, "sets", "NES", "MAME/nes")
        .unwrap();
    env.store
        .audit_source_set(&env.config, "NES", None, AuditEvidence::Incremental)
        .unwrap();
    env.store.set_follow("NES", true).unwrap();
    env.serve_mame("mame0270");
    env.serve_support(COMMIT_A, "0.270");

    let report = env
        .fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(report.stopped, None, "{report:?}");
    let added = Some(FetchOutcome::Added);
    assert_eq!(
        outcomes(&report),
        [
            ("mame".into(), FetchArtifact::Release, added),
            ("mame".into(), FetchArtifact::Dat, added),
            ("nes".into(), FetchArtifact::Dat, added),
        ]
        .into_iter()
        .chain(support_outcomes(FetchOutcome::Added))
        .collect::<Vec<_>>(),
        "the ListXML it brought brings the support files after it"
    );
    assert_eq!(report.attempts[1].version.as_deref(), Some("mame0270"));
    assert_eq!(
        report.attempts[2].version.as_deref(),
        Some("mame0270"),
        "a software list, whose header has no version, takes the release's tag"
    );
    assert!(
        report.attempts[..3]
            .iter()
            .all(|a| a.release.as_deref() == Some("mame0270"))
    );
    assert_eq!(env.store.followed_revisions("Arcade").unwrap().len(), 1);
    assert_eq!(env.store.followed_revisions("NES").unwrap().len(), 1);
    assert_eq!(
        env.pace.waits(),
        vec![MIN_GAP; env.fake.seen().len()],
        "a second before each"
    );

    // A day later the release is unchanged, so only the release is checked.
    env.backdate(25);
    let again = env
        .fetch(FetchProvider::Mame, FetchTrigger::Schedule)
        .unwrap();
    assert_eq!(
        outcomes(&again),
        [(
            "mame".into(),
            FetchArtifact::Release,
            Some(FetchOutcome::Unchanged)
        )]
    );
}

#[test]
fn a_deleted_software_list_revision_is_fetched_again_for_the_release_it_came_from() {
    let env = Env::new(MAME);
    env.store
        .import_dat_bytes(&software_list("nes", "0"), "nes.xml", None)
        .unwrap();
    for set in ["NES", "NES again"] {
        std::fs::create_dir_all(env._tmp.path().join("sets").join(set)).unwrap();
    }
    env.store
        .add_source_set(&env.config, "sets", "NES", "MAME/nes")
        .unwrap();
    env.store.set_follow("NES", true).unwrap();
    env.serve_mame("mame0270");
    let first = env
        .fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    let fetched = first.attempts[1].dat_revision.clone().unwrap();
    env.store.unregister_source_set(&env.config, "NES").unwrap();
    env.store.delete_dat_revision(&fetched).unwrap();
    env.store
        .add_source_set(&env.config, "sets", "NES again", "MAME/nes")
        .unwrap();
    env.store.set_follow("NES again", true).unwrap();

    // A day later the release is unchanged, but the software list it brought was deleted, so it is brought again.
    env.backdate(25);
    let again = env
        .fetch(FetchProvider::Mame, FetchTrigger::Schedule)
        .unwrap();
    assert_eq!(
        outcomes(&again),
        [
            (
                "mame".into(),
                FetchArtifact::Release,
                Some(FetchOutcome::Unchanged)
            ),
            ("nes".into(), FetchArtifact::Dat, Some(FetchOutcome::Added)),
        ]
    );
    assert_eq!(
        again.attempts[1].dat_revision.as_deref(),
        Some(fetched.as_str())
    );
    assert!(
        env.store
            .dat_revisions()
            .unwrap()
            .iter()
            .any(|r| r.sha256 == fetched && r.blob_present)
    );
}

#[test]
fn a_mame_release_asset_outside_the_official_release_downloads_is_refused() {
    let env = Env::new(MAME);
    env.store
        .import_dat_bytes(&listxml("mame0001"), "mame.xml", None)
        .unwrap();
    std::fs::create_dir_all(env._tmp.path().join("sets/Arcade")).unwrap();
    env.store
        .add_source_set(&env.config, "sets", "Arcade", "MAME/MAME")
        .unwrap();
    env.store.set_follow("Arcade", true).unwrap();
    let release = r#"{"tag_name":"mame0270","assets":[{"name":"mame0270lx.zip",
        "browser_download_url":"https://mirror.example/mame0270lx.zip"}]}"#;
    env.fake.reply(
        "/repos/mamedev/mame/releases/latest",
        Reply::Body(release.as_bytes().to_vec()),
    );
    let report = env
        .fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(
        outcomes(&report),
        [(
            "mame".into(),
            FetchArtifact::Release,
            Some(FetchOutcome::Failed)
        )]
    );
    assert_eq!(env.fake.seen().len(), 1);
}

/// The mirror MAME's support files are fetched from.
const MIRROR: &str = "/AntoPISA/MAME_SupportFiles";

/// A support file as progetto-SNAPS publishes it, with its `;; … MAME 0.NNN ;;` header, for MAME `version`.
fn support_ini(file: &str, version: &str) -> Vec<u8> {
    format!(
        "[FOLDER_SETTINGS]\r\nRootFolderIcon mame\r\nSubFolderIcon folder\r\n\r\n\
         ;; {file} {version} / 21-Aug-26 / MAME {version} ;;\r\n\r\n[ROOT_FOLDER]\r\n\r\n[Category]\r\n\
         pong=Sports / Tennis\r\n"
    )
    .into_bytes()
}

const SUPPORT: [&str; 3] = ["catver.ini", "languages.ini", "bestgames.ini"];

impl Env {
    /// Imports the ListXML of `build` and registers the Source Set `Arcade` bound to it, not Following.
    fn add_arcade(&self, build: &str) {
        self.store
            .import_dat_bytes(&listxml(build), &format!("{build}.xml"), None)
            .unwrap();
        if self.store.source_set("Arcade").is_err() {
            std::fs::create_dir_all(self._tmp.path().join("sets/Arcade")).unwrap();
            self.store
                .add_source_set(&self.config, "sets", "Arcade", "MAME/MAME")
                .unwrap();
        }
    }

    /// Serves the mirror at `commit`, its support files naming MAME `version`.
    fn serve_support(&self, commit: &str, version: &str) {
        self.fake.reply(
            &format!("/repos{MIRROR}/commits/main"),
            Reply::Body(commit.as_bytes().to_vec()),
        );
        for file in SUPPORT {
            self.fake.reply(
                &format!("{MIRROR}/{commit}/{file}/{file}"),
                Reply::Body(support_ini(file, version)),
            );
        }
    }

    /// The paths of the requests sent to the mirror, through the API or for its files.
    fn support_requests(&self) -> Vec<String> {
        self.fake
            .seen()
            .into_iter()
            .map(|s| s.path)
            .filter(|p| p.contains("MAME_SupportFiles"))
            .collect()
    }
}

const COMMIT_A: &str = "ad2049f2b06181c0e2dabda710f3b712cf97d36d";
const COMMIT_B: &str = "bbeff16968f34ee51340f5948598462d88ed07d5";

fn support_outcomes(outcome: FetchOutcome) -> Vec<(String, FetchArtifact, Option<FetchOutcome>)> {
    [
        FetchArtifact::SupportCommit,
        FetchArtifact::Catver,
        FetchArtifact::Languages,
        FetchArtifact::Bestgames,
    ]
    .into_iter()
    .map(|a| (SUPPORT_SYSTEM.to_string(), a, Some(outcome)))
    .collect()
}

#[test]
fn a_new_listxml_brings_the_support_files_at_the_mirrors_commit_once_and_not_again_the_same_day() {
    let env = Env::new(MAME);
    env.add_arcade("mame0289");
    env.serve_support(COMMIT_A, "0.289");

    let report = env
        .fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(report.stopped, None, "{report:?}");
    assert_eq!(outcomes(&report), support_outcomes(FetchOutcome::Added));
    assert!(
        report
            .attempts
            .iter()
            .all(|a| a.commit.as_deref() == Some(COMMIT_A)),
        "{report:?}"
    );
    assert_eq!(
        report.attempts[1..]
            .iter()
            .map(|a| a.version.as_deref())
            .collect::<Vec<_>>(),
        [Some("0.289"); 3],
        "each file's version is the one its header names"
    );
    assert_eq!(
        env.support_requests(),
        [
            format!("/repos{MIRROR}/commits/main"),
            format!("{MIRROR}/{COMMIT_A}/catver.ini/catver.ini"),
            format!("{MIRROR}/{COMMIT_A}/languages.ini/languages.ini"),
            format!("{MIRROR}/{COMMIT_A}/bestgames.ini/bestgames.ini"),
        ]
    );
    assert_eq!(
        env.pace.waits(),
        vec![MIN_GAP; env.fake.seen().len()],
        "a second before each"
    );
    let agents: Vec<_> = env.fake.seen().into_iter().map(|s| s.user_agent).collect();
    assert!(
        agents
            .iter()
            .all(|a| a.as_deref().is_some_and(|a| a.starts_with("title-select/"))),
        "{agents:?}"
    );

    let kept = env.store.support_files().unwrap();
    let mut kinds: Vec<_> = kept
        .iter()
        .map(|f| {
            (
                f.kind,
                f.mame_version.as_str(),
                f.commit.as_deref(),
                f.origin,
            )
        })
        .collect();
    kinds.sort_by_key(|k| k.0);
    assert_eq!(
        kinds,
        [
            (
                SupportFileKind::Catver,
                "0.289",
                Some(COMMIT_A),
                SupportFileOrigin::Fetch
            ),
            (
                SupportFileKind::Languages,
                "0.289",
                Some(COMMIT_A),
                SupportFileOrigin::Fetch
            ),
            (
                SupportFileKind::Bestgames,
                "0.289",
                Some(COMMIT_A),
                SupportFileOrigin::Fetch
            ),
        ]
    );
    for file in &kept {
        assert!(
            env.store
                .dir()
                .join("support_files")
                .join(&file.sha256)
                .is_file()
        );
    }

    // The same day, nothing more is sent, whatever starts the fetch.
    let seen = env.fake.seen().len();
    let again = env
        .fetch(FetchProvider::Mame, FetchTrigger::Schedule)
        .unwrap();
    assert!(again.attempts.is_empty(), "{again:?}");
    assert!(matches!(
        env.fetch(FetchProvider::Mame, FetchTrigger::Manual),
        Err(Error::Refused(_))
    ));
    // Nor when a newer ListXML arrives that day: the files wait for the day to pass.
    env.add_arcade("mame0290");
    let newer = env
        .fetch(FetchProvider::Mame, FetchTrigger::Schedule)
        .unwrap();
    assert!(newer.attempts.is_empty(), "{newer:?}");
    assert_eq!(env.fake.seen().len(), seen);

    // A day later the newer ListXML brings them again, from the commit the mirror is at then.
    env.backdate(25);
    env.serve_support(COMMIT_B, "0.290");
    let later = env
        .fetch(FetchProvider::Mame, FetchTrigger::Schedule)
        .unwrap();
    assert_eq!(outcomes(&later), support_outcomes(FetchOutcome::Added));
    assert!(
        later
            .attempts
            .iter()
            .all(|a| a.commit.as_deref() == Some(COMMIT_B))
    );
    assert_eq!(
        env.store.support_files().unwrap().len(),
        6,
        "the earlier files are kept too"
    );

    // With no newer ListXML, another day brings nothing.
    env.backdate(25);
    let none = env
        .fetch(FetchProvider::Mame, FetchTrigger::Schedule)
        .unwrap();
    assert!(none.attempts.is_empty(), "{none:?}");
}

#[test]
fn an_unchanged_mirror_brings_the_same_files_unchanged() {
    let env = Env::new(MAME);
    env.add_arcade("mame0289");
    env.serve_support(COMMIT_A, "0.289");
    env.fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    env.backdate(25);
    env.add_arcade("mame0290");
    let again = env
        .fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(outcomes(&again), support_outcomes(FetchOutcome::Unchanged));
    assert_eq!(env.store.support_files().unwrap().len(), 3);
}

#[test]
fn nothing_is_fetched_for_the_support_files_while_mame_fetching_is_off() {
    let env = Env::new(REDUMP);
    env.add_arcade("mame0289");
    env.serve_support(COMMIT_A, "0.289");
    env.serve_psx("2");
    assert!(matches!(
        env.fetch(FetchProvider::Mame, FetchTrigger::Schedule),
        Err(Error::Refused(_))
    ));
    env.fetch(FetchProvider::Redump, FetchTrigger::Manual)
        .unwrap();
    assert!(env.support_requests().is_empty());
    assert!(env.store.support_files().unwrap().is_empty());
}

#[test]
fn nothing_is_fetched_for_the_support_files_without_a_listxml_source_set() {
    let env = Env::new(MAME);
    // A ListXML no Source Set is bound to.
    env.store
        .import_dat_bytes(&listxml("mame0289"), "mame.xml", None)
        .unwrap();
    env.serve_support(COMMIT_A, "0.289");
    let report = env
        .fetch(FetchProvider::Mame, FetchTrigger::Schedule)
        .unwrap();
    assert!(report.attempts.is_empty(), "{report:?}");
    assert!(env.support_requests().is_empty());
}

#[test]
fn a_failed_support_file_stops_the_fetch_keeps_the_last_good_files_and_is_shown() {
    let env = Env::new(MAME);
    env.add_arcade("mame0289");
    env.serve_support(COMMIT_A, "0.289");
    env.fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    let good = env.store.support_files().unwrap();

    env.backdate(25);
    env.add_arcade("mame0290");
    env.serve_support(COMMIT_B, "0.290");
    env.fake.reply(
        &format!("{MIRROR}/{COMMIT_B}/languages.ini/languages.ini"),
        Reply::Status(503),
    );
    let report = env
        .fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(
        outcomes(&report),
        [
            (
                SUPPORT_SYSTEM.into(),
                FetchArtifact::SupportCommit,
                Some(FetchOutcome::Added)
            ),
            (
                SUPPORT_SYSTEM.into(),
                FetchArtifact::Catver,
                Some(FetchOutcome::Added)
            ),
            (
                SUPPORT_SYSTEM.into(),
                FetchArtifact::Languages,
                Some(FetchOutcome::Failed)
            ),
        ]
    );
    assert!(
        report.stopped.as_deref().is_some_and(|e| e.contains("503")),
        "{report:?}"
    );
    let kept = env.store.support_files().unwrap();
    for file in &good {
        assert!(kept.contains(file), "{file:?} is kept");
        assert!(
            env.store
                .dir()
                .join("support_files")
                .join(&file.sha256)
                .is_file()
        );
    }

    let status = env.store.fetch_status(&env.fetcher).unwrap();
    let mame = status
        .providers
        .iter()
        .find(|p| p.provider == FetchProvider::Mame)
        .unwrap();
    let support = mame.support.as_ref().unwrap();
    assert_eq!(support.source_sets, ["Arcade"]);
    assert!(!support.fetched_for_listxml);
    let languages = support
        .artifacts
        .iter()
        .find(|a| a.artifact == FetchArtifact::Languages)
        .unwrap();
    assert_eq!(
        languages.last_attempt.as_ref().unwrap().outcome,
        Some(FetchOutcome::Failed)
    );
    assert_eq!(
        languages.last_success.as_ref().unwrap().version.as_deref(),
        Some("0.289"),
        "the last good file stands"
    );

    // The next day all of them are fetched again, as they have not all been fetched for the newer ListXML.
    env.backdate(25);
    env.serve_support(COMMIT_B, "0.290");
    let retry = env
        .fetch(FetchProvider::Mame, FetchTrigger::Schedule)
        .unwrap();
    assert_eq!(
        outcomes(&retry),
        [
            (
                SUPPORT_SYSTEM.into(),
                FetchArtifact::SupportCommit,
                Some(FetchOutcome::Unchanged)
            ),
            (
                SUPPORT_SYSTEM.into(),
                FetchArtifact::Catver,
                Some(FetchOutcome::Unchanged)
            ),
            (
                SUPPORT_SYSTEM.into(),
                FetchArtifact::Languages,
                Some(FetchOutcome::Added)
            ),
            (
                SUPPORT_SYSTEM.into(),
                FetchArtifact::Bestgames,
                Some(FetchOutcome::Added)
            ),
        ]
    );
    let status = env.store.fetch_status(&env.fetcher).unwrap();
    let mame = status
        .providers
        .iter()
        .find(|p| p.provider == FetchProvider::Mame)
        .unwrap();
    assert!(mame.support.as_ref().unwrap().fetched_for_listxml);
}

#[test]
fn a_support_file_that_is_not_the_one_asked_for_or_names_no_mame_version_fails() {
    for (body, why) in [
        (support_ini("languages.ini", "0.289"), "languages.ini"),
        (b"[Category]\r\npong=Sports / Tennis\r\n".to_vec(), "header"),
        (support_ini("catver.ini", "soon"), "MAME version"),
    ] {
        let env = Env::new(MAME);
        env.add_arcade("mame0289");
        env.serve_support(COMMIT_A, "0.289");
        env.fake.reply(
            &format!("{MIRROR}/{COMMIT_A}/catver.ini/catver.ini"),
            Reply::Body(body),
        );
        let report = env
            .fetch(FetchProvider::Mame, FetchTrigger::Manual)
            .unwrap();
        assert_eq!(report.attempts[1].outcome, Some(FetchOutcome::Failed));
        let error = report.stopped.unwrap();
        assert!(error.contains(why), "{why}: {error}");
        assert!(env.store.support_files().unwrap().is_empty());
    }
}

#[test]
fn a_commit_check_that_answers_no_commit_fails() {
    let env = Env::new(MAME);
    env.add_arcade("mame0289");
    env.serve_support(COMMIT_A, "0.289");
    env.fake.reply(
        &format!("/repos{MIRROR}/commits/main"),
        Reply::Body(b"<html>".to_vec()),
    );
    let report = env
        .fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    assert_eq!(
        outcomes(&report),
        [(
            SUPPORT_SYSTEM.into(),
            FetchArtifact::SupportCommit,
            Some(FetchOutcome::Failed)
        )]
    );
    assert_eq!(env.support_requests().len(), 1);
}

#[test]
fn the_commit_is_not_checked_while_any_support_file_waits_out_its_day() {
    let env = Env::new(MAME);
    env.add_arcade("mame0289");
    env.serve_support(COMMIT_A, "0.289");
    env.fetch(FetchProvider::Mame, FetchTrigger::Manual)
        .unwrap();
    env.backdate(25);
    env.add_arcade("mame0290");
    // A request for catver.ini recorded since, as another Service could.
    let db = rusqlite::Connection::open(env.store.dir().join("title-select.sqlite")).unwrap();
    db.execute(
        "INSERT INTO fetches (provider, system, artifact, trigger, actor, url, started_at, finished_at, outcome)
         VALUES ('mame', ?1, 'catver', 'manual', 'Operator', 'x', ?2, ?2, 'failed')",
        rusqlite::params![SUPPORT_SYSTEM, jiff::Timestamp::now().to_string()],
    )
    .unwrap();
    let seen = env.support_requests().len();
    let report = env
        .fetch(FetchProvider::Mame, FetchTrigger::Schedule)
        .unwrap();
    assert!(report.attempts.is_empty(), "{report:?}");
    assert_eq!(env.support_requests().len(), seen);
}
