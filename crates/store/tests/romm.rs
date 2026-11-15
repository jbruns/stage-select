//! The RomM client and its connection test, against a stub RomM on a local port that answers as RomM 5.4.1 does
//! (research 0136). No test sends a request to a real RomM.

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
