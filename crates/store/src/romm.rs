//! RomM, an optional integration that is off by default (ADR 0020): its connection as stored, a small client for its
//! API, testing the connection, the RomM platform each Library Folder maps to, and the scan a Build requests.

use std::path::{Component, Path};

use std::sync::{Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use rusqlite::{OptionalExtension, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use title_select_api::build::CleanupAction;
use title_select_api::romm::{
    RommCheck, RommFolderPlatform, RommPlatform, RommScan, RommScanOutcome, RommTestRequest,
    RommUpdate, RommUser, RommView, version_tested,
};
use title_select_api::settings::SecretUpdate;
use title_select_api::{Job, JobKind};

use crate::config::Config;
use crate::fetch::{Http, Method, refusal_unnamed};
use crate::{Error, Result, Store, now};

pub use crate::romm_additions::Released;

/// RomM's address as given, checked: plain `http` or `https` to a named host, with no credentials, query or fragment,
/// and without a trailing `/`. A path is kept, for a RomM served under one.
pub fn base_url(url: &str) -> std::result::Result<String, String> {
    let url = url.trim();
    let why = refusal_unnamed(url).or_else(|| {
        url.contains(['?', '#'])
            .then_some("it has a query or fragment; give only RomM's address")
    });
    match why {
        Some(why) => Err(format!("RomM's address is refused: {why}.")),
        None => Ok(url.trim_end_matches('/').to_owned()),
    }
}

/// The RomM platform the Library Folder at `folder` maps to, given the RomM root at `romm_root`: the folder's one
/// component under the root is its `fs_slug`. Both paths must already be resolved to where they really are on disk
/// (see [`Romm::platform`]): they are compared as given, and a `..` is never collapsed, since across a symlink that
/// would name another folder.
pub fn platform(romm_root: Option<&Path>, folder: &Path) -> RommPlatform {
    let Some(romm_root) = romm_root else {
        return RommPlatform::NoRoot;
    };
    let Ok(under) = folder.strip_prefix(romm_root) else {
        return RommPlatform::OutsideRoot;
    };
    let parts: Vec<_> = under.components().collect();
    match parts[..] {
        _ if parts.iter().any(|p| !matches!(p, Component::Normal(_))) => RommPlatform::OutsideRoot,
        [] => RommPlatform::IsRoot,
        [Component::Normal(slug)] => RommPlatform::Mapped {
            fs_slug: slug.to_string_lossy().into_owned(),
        },
        _ => RommPlatform::Nested,
    }
}

/// The RomM connection as stored, its token included.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Romm {
    pub enabled: bool,
    /// RomM's address, such as `https://<domain>`, without a trailing `/`.
    pub base_url: Option<String>,
    /// The Client API Token, in plain text. It is never returned or logged.
    pub token: Option<String>,
    /// The Root RomM mounts as its library, `roms/`.
    pub root: Option<String>,
}

impl std::fmt::Debug for Romm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Romm")
            .field("enabled", &self.enabled)
            .field("base_url", &self.base_url)
            .field("token_set", &self.token.is_some())
            .field("root", &self.root)
            .finish()
    }
}

impl Romm {
    /// Whether Title Select talks to RomM: it is turned on, with an address, a token and a root.
    pub fn is_on(&self) -> bool {
        self.enabled && self.base_url.is_some() && self.token.is_some() && self.root.is_some()
    }

    /// This connection with `update` applied, checked against `config`.
    pub(crate) fn updated(&self, config: &Config, update: &RommUpdate) -> Result<Romm> {
        let refuse = |why: String| Err(Error::Refused(why));
        let base_url = match update.base_url.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(url) => Some(base_url(url).map_err(Error::Refused)?),
        };
        let token = match &update.token {
            SecretUpdate::Keep => {
                if self.token.is_some() && base_url != self.base_url {
                    return refuse(
                        "Enter your RomM token again when you change RomM's address, so that your saved token is \
                         never sent to an address you didn't save it with."
                            .into(),
                    );
                }
                self.token.clone()
            }
            SecretUpdate::Clear => None,
            SecretUpdate::Set(token) => match token.trim() {
                "" => return refuse("The RomM token is empty.".into()),
                token => Some(token.to_owned()),
            },
        };
        let root = match update.root.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(name) => match config.root(name) {
                None => return refuse(format!("{name} is not a root in your configuration.")),
                Some(root) if root.is_ingest() => {
                    return refuse(format!(
                        "{name} is an ingest folder. Choose the root that holds your library folders, the one RomM \
                         mounts as its library."
                    ));
                }
                Some(_) => Some(name.to_owned()),
            },
        };
        if update.enabled {
            if base_url.is_none() {
                return refuse("To turn RomM on, give its address.".into());
            }
            if token.is_none() {
                return refuse("To turn RomM on, give it a token.".into());
            }
            if root.is_none() {
                return refuse("To turn RomM on, choose the root it mounts as its library.".into());
            }
        }
        Ok(Romm {
            enabled: update.enabled,
            base_url,
            token,
            root,
        })
    }

    /// The RomM platform the Library Folder at `path` in the Root `root` maps to, by the RomM root saved here. Both
    /// are resolved to where they really are on disk, symlinks followed, so that two spellings of one folder agree
    /// and a `..` past a symlink isn't taken for its parent as written; either missing from disk is
    /// [`RommPlatform::Unresolved`].
    pub fn platform(&self, config: &Config, root: &str, path: &str) -> RommPlatform {
        let Some(romm_root) = self.root.as_deref().and_then(|name| config.root(name)) else {
            return RommPlatform::NoRoot;
        };
        let Some(root) = config.root(root) else {
            return RommPlatform::OutsideRoot;
        };
        match (
            std::fs::canonicalize(&romm_root.path),
            std::fs::canonicalize(root.path.join(path)),
        ) {
            (Ok(romm_root), Ok(folder)) => platform(Some(&romm_root), &folder),
            _ => RommPlatform::Unresolved,
        }
    }

    /// A client for testing this connection, or the address and token `request` gives in its place. The saved token
    /// is sent only to the saved address.
    pub fn tester(&self, request: &RommTestRequest) -> Result<RommClient> {
        let base_url = match request.base_url.as_deref().map(str::trim) {
            None | Some("") => self.base_url.clone(),
            Some(url) => Some(base_url(url).map_err(Error::Refused)?),
        };
        let Some(base_url) = base_url else {
            return Err(Error::Refused("Give RomM's address to test it.".into()));
        };
        let token = match &request.token {
            SecretUpdate::Keep
                if self.token.is_some() && Some(&base_url) != self.base_url.as_ref() =>
            {
                return Err(Error::Refused(
                    "Enter your RomM token again to test a new address: your saved token is sent only to the \
                     address you saved it with."
                        .into(),
                ));
            }
            SecretUpdate::Keep => self.token.clone(),
            SecretUpdate::Clear => None,
            SecretUpdate::Set(token) => Some(token.trim().to_owned()).filter(|t| !t.is_empty()),
        };
        RommClient::new(&base_url, token.as_deref()).map_err(|e| Error::Refused(e.to_string()))
    }

    pub fn view(&self) -> RommView {
        RommView {
            enabled: self.enabled,
            base_url: self.base_url.clone(),
            token_set: self.token.is_some(),
            root: self.root.clone(),
        }
    }
}

/// How long one request to RomM may take unless the caller says otherwise.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// The largest answer read from RomM unless the caller says otherwise.
pub const BODY_LIMIT: u64 = 16 << 20;

/// Why a request to RomM failed. Its message never names the token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RommError {
    /// The address is one Title Select won't send a token to.
    #[error("{0}")]
    Address(String),
    /// RomM didn't answer: it is down, its address is wrong, or it took longer than the timeout.
    #[error("RomM can't be reached: {0}")]
    Unreachable(String),
    /// HTTP 401: no token was sent, or RomM doesn't accept it.
    #[error(
        "RomM refused the token: it may be mistyped, expired or revoked, or its user turned off. Create a new one in \
         RomM and save it here"
    )]
    Unauthorized,
    /// HTTP 403: the token lacks a scope the request needs.
    #[error("RomM refused {path}: the token lacks a scope it needs")]
    Forbidden { path: String },
    /// Any other status that isn't a success, such as 404, or 409 while RomM is already scanning.
    #[error("RomM answered HTTP {status} to {path}")]
    Status { path: String, status: u16 },
    /// A success whose body isn't what RomM sends.
    #[error("RomM's answer to {path} doesn't read: {why}")]
    Malformed { path: String, why: String },
}

/// A small client for RomM's API: one address, a bearer token, and a timeout for each request. It follows no
/// redirect, and never logs the token.
pub struct RommClient {
    http: Http,
    base_url: String,
    token: Option<String>,
    timeout: Duration,
    limit: u64,
}

impl std::fmt::Debug for RommClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RommClient")
            .field("base_url", &self.base_url)
            .field("token_set", &self.token.is_some())
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// What RomM's heartbeat says of itself.
#[derive(Deserialize)]
struct Heartbeat {
    #[serde(rename = "SYSTEM")]
    system: HeartbeatSystem,
}

#[derive(Deserialize)]
struct HeartbeatSystem {
    #[serde(rename = "VERSION")]
    version: String,
}

/// The fields of RomM's user that Title Select reads.
#[derive(Deserialize)]
struct Me {
    username: String,
    role: String,
}

/// How each of [`title_select_api::romm::REQUIRED_SCOPES`] is checked: a request that only that scope admits, and
/// that changes nothing. A token lacking the scope is answered 403; one holding it is answered, or told the made-up id
/// isn't found.
const PROBES: [(&str, Method, &str); 5] = [
    ("roms.read", Method::Get, "/api/roms?limit=1"),
    ("platforms.read", Method::Get, "/api/platforms/identifiers"),
    (
        "collections.read",
        Method::Get,
        "/api/collections/identifiers",
    ),
    // Adds no ROM to collection 0, which RomM never creates.
    ("collections.write", Method::Post, "/api/collections/0/roms"),
    (
        "tasks.run",
        Method::Get,
        "/api/tasks/title-select-scope-check",
    ),
];

impl RommClient {
    /// A client for the RomM at `base_url`, sending `token` as its bearer token if given.
    pub fn new(base_url: &str, token: Option<&str>) -> std::result::Result<RommClient, RommError> {
        Ok(RommClient {
            http: Http::new(),
            base_url: self::base_url(base_url).map_err(RommError::Address)?,
            token: token.map(str::to_owned),
            timeout: TIMEOUT,
            limit: BODY_LIMIT,
        })
    }

    /// A client for the saved connection, if it has an address.
    pub fn of(romm: &Romm) -> Option<std::result::Result<RommClient, RommError>> {
        let base_url = romm.base_url.as_deref()?;
        Some(RommClient::new(base_url, romm.token.as_deref()))
    }

    /// The longest each request may take.
    pub fn with_timeout(mut self, timeout: Duration) -> RommClient {
        self.timeout = timeout;
        self
    }

    /// The largest answer each request reads.
    pub fn with_body_limit(mut self, limit: u64) -> RommClient {
        self.limit = limit;
        self
    }

    /// Sends `method` to `path`, such as `/api/heartbeat`, with `body` as JSON if given, and returns the body of a
    /// success.
    pub fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> std::result::Result<Vec<u8>, RommError> {
        let body = body.map(|b| serde_json::to_vec(b).expect("JSON serializes"));
        self.send_body(
            method,
            path,
            body.as_deref().map(|b| ("application/json", b)),
        )
    }

    /// Sends `method` to `path` with `body`, of its content type, if given, and returns the body of a success.
    pub fn send_body(
        &self,
        method: Method,
        path: &str,
        body: Option<(&str, &[u8])>,
    ) -> std::result::Result<Vec<u8>, RommError> {
        let url = format!("{}{path}", self.base_url);
        let bearer = self.token.as_ref().map(|t| format!("Bearer {t}"));
        let mut headers = vec![("Accept", "application/json")];
        if let Some(bearer) = &bearer {
            headers.push(("Authorization", bearer));
        }
        if let Some((content_type, _)) = body {
            headers.push(("Content-Type", content_type));
        }
        let body = body.map(|(_, b)| b);
        let (status, bytes) = self
            .http
            .send(method, &url, &headers, body, self.timeout, self.limit)
            .map_err(RommError::Unreachable)?;
        match status {
            200..=299 => Ok(bytes),
            401 => Err(RommError::Unauthorized),
            403 => Err(RommError::Forbidden { path: path.into() }),
            status => Err(RommError::Status {
                path: path.into(),
                status,
            }),
        }
    }

    pub(crate) fn read<T: DeserializeOwned>(
        path: &str,
        bytes: &[u8],
    ) -> std::result::Result<T, RommError> {
        serde_json::from_slice(bytes).map_err(|e| RommError::Malformed {
            path: path.into(),
            why: e.to_string(),
        })
    }

    /// Gets `path` and reads its JSON.
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> std::result::Result<T, RommError> {
        Self::read(path, &self.send(Method::Get, path, None)?)
    }

    /// Posts `body` as JSON to `path` and reads the JSON it answers.
    pub fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> std::result::Result<T, RommError> {
        let body = serde_json::to_value(body).expect("JSON serializes");
        Self::read(path, &self.send(Method::Post, path, Some(&body))?)
    }

    /// The version RomM's heartbeat reports. The heartbeat needs no token.
    pub fn version(&self) -> std::result::Result<String, RommError> {
        Ok(self.get::<Heartbeat>("/api/heartbeat")?.system.version)
    }

    /// The user the token belongs to, if the token may read its own profile (`me.read`).
    pub fn user(&self) -> std::result::Result<Option<RommUser>, RommError> {
        match self.get::<Me>("/api/users/me") {
            Ok(me) => Ok(Some(RommUser {
                username: me.username,
                admin: me.role == "admin",
            })),
            Err(RommError::Forbidden { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Whether the token lacks `scope`, one of [`title_select_api::romm::REQUIRED_SCOPES`], by the request that
    /// tests it while changing nothing.
    pub fn lacks(&self, scope: &str) -> std::result::Result<bool, RommError> {
        let (_, method, path) = PROBES
            .into_iter()
            .find(|(s, ..)| *s == scope)
            .expect("a required scope");
        let body = (method == Method::Post).then(|| serde_json::json!({"rom_ids": []}));
        match self.send(method, path, body.as_ref()) {
            Ok(_) | Err(RommError::Status { status: 404, .. }) => Ok(false),
            Err(RommError::Forbidden { .. }) => Ok(true),
            Err(e) => Err(e),
        }
    }

    /// Tests the connection: RomM's version, who the token belongs to, and which required scopes it lacks. It stops
    /// at the first failure, keeping what it found until then.
    pub fn check(&self) -> RommCheck {
        let mut check = RommCheck::default();
        let mut run = || -> std::result::Result<(), RommError> {
            let version = self.version()?;
            check.version_tested = version_tested(&version);
            check.version = Some(version);
            if self.token.is_none() {
                return Err(RommError::Unauthorized);
            }
            check.user = self.user()?;
            for (scope, ..) in PROBES {
                if self.lacks(scope)? {
                    check.missing_scopes.push(scope.into());
                }
            }
            Ok(())
        };
        if let Err(e) = run() {
            check.failure = Some(if self.token.is_none() && e == RommError::Unauthorized {
                "No RomM token is set: create one in RomM and save it here.".into()
            } else {
                format!("{e}.")
            });
        }
        check
    }
}

/// How a scan request waits on RomM: how often it polls the scan's task, how long it backs off while RomM is busy with
/// another scan (doubling from `backoff` up to `backoff_max`), how long it waits for both together, and how long it
/// waits for RomM to answer each request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanTiming {
    pub poll: Duration,
    pub backoff: Duration,
    pub backoff_max: Duration,
    pub window: Duration,
    pub request: Duration,
}

impl Default for ScanTiming {
    /// Polls every 5 seconds and backs off from 5 seconds to a minute, for up to 10 minutes (#136).
    fn default() -> ScanTiming {
        ScanTiming {
            poll: Duration::from_secs(5),
            backoff: Duration::from_secs(5),
            backoff_max: Duration::from_secs(60),
            window: Duration::from_secs(600),
            request: TIMEOUT,
        }
    }
}

/// How a scan ended: its outcome, RomM's id for its task if it accepted it, and why, for a warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanEnd {
    pub outcome: RommScanOutcome,
    pub task_id: Option<String>,
    pub detail: Option<String>,
}

/// The fields of RomM's answer to a scan request, or to a poll of its task, that Title Select reads.
#[derive(Deserialize)]
struct Task {
    task_id: String,
    #[serde(default)]
    status: Option<String>,
}

/// A span of time as a person would say it: "10 minutes", "45 seconds", "300 ms".
fn span(d: Duration) -> String {
    match d.as_secs() {
        0 => format!("{} ms", d.as_millis()),
        s if s % 60 == 0 => match s / 60 {
            1 => "1 minute".into(),
            m => format!("{m} minutes"),
        },
        1 => "1 second".into(),
        s => format!("{s} seconds"),
    }
}

/// Tells a scan's follower to stop, as when the Service stops: it then stops waiting at once and records the scan as
/// no longer followed.
#[derive(Debug, Default)]
pub struct ScanStop {
    stopped: Mutex<bool>,
    signal: Condvar,
}

impl ScanStop {
    pub fn stop(&self) {
        *self.stopped.lock().unwrap_or_else(|e| e.into_inner()) = true;
        self.signal.notify_all();
    }

    pub fn is_stopped(&self) -> bool {
        *self.stopped.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits `span`, or less if told to stop meanwhile. Returns whether it was told to stop.
    pub fn wait(&self, span: Duration) -> bool {
        let stopped = self.stopped.lock().unwrap_or_else(|e| e.into_inner());
        let (stopped, _) = self
            .signal
            .wait_timeout_while(stopped, span, |stopped| !*stopped)
            .unwrap_or_else(|e| e.into_inner());
        *stopped
    }
}

/// Why a scan stopped being followed, said of the platform `fs_slug`, before RomM accepted it or after.
fn stopped(fs_slug: &str, accepted: bool, why: &str) -> String {
    match accepted {
        false => format!("{why}, so RomM wasn't asked to scan {fs_slug}. Scan it in RomM."),
        true => format!(
            "{why}, so whether RomM finished scanning {fs_slug} isn't known. Check it in RomM."
        ),
    }
}

/// Requests a quick scan of the RomM platform `fs_slug` of the RomM `was` and waits for its task to end, within
/// `timing`'s window or until `stop`. While RomM is busy with another scan (HTTP 409), it asks again, backing off.
/// While RomM can't be reached during the wait, it keeps polling. Each request reads the RomM settings as they are
/// then: one sends the token saved now, and none is sent once RomM is off or its address or root is no longer `was`'s.
/// It holds the Store only to read the settings, never while it waits on RomM, and returns `None` once the Store is
/// gone.
fn follow_romm_scan(
    store: &Weak<Store>,
    was: &Romm,
    fs_slug: &str,
    timing: &ScanTiming,
    stop: &ScanStop,
) -> Option<ScanEnd> {
    let deadline = Instant::now() + timing.window;
    let end = |outcome, task_id: Option<&str>, detail: String| {
        Some(ScanEnd {
            outcome,
            task_id: task_id.map(str::to_owned),
            detail: Some(detail),
        })
    };
    let halted = |task_id: Option<&str>| {
        end(
            RommScanOutcome::Stopped,
            task_id,
            stopped(fs_slug, task_id.is_some(), "Title Select stopped"),
        )
    };
    let client = |task_id: Option<&str>| -> std::result::Result<RommClient, Option<ScanEnd>> {
        if stop.is_stopped() {
            return Err(halted(task_id));
        }
        let accepted = task_id.is_some();
        let settings = store.upgrade().ok_or(None)?.settings();
        let now = match settings {
            Ok(settings) => settings.romm,
            Err(e) => {
                return Err(end(
                    RommScanOutcome::Error,
                    task_id,
                    stopped(
                        fs_slug,
                        accepted,
                        &format!("Your RomM settings can't be read: {e}"),
                    ),
                ));
            }
        };
        let why = if !now.is_on() {
            Some("You turned RomM off meanwhile")
        } else if now.base_url != was.base_url {
            Some("You changed RomM's address meanwhile")
        } else if now.root != was.root {
            Some("You changed the root RomM mounts as its library meanwhile")
        } else {
            None
        };
        if let Some(why) = why {
            return Err(end(
                RommScanOutcome::Stopped,
                task_id,
                stopped(fs_slug, accepted, why),
            ));
        }
        match RommClient::of(&now).expect("RomM that is on has an address") {
            Ok(client) => Ok(client.with_timeout(timing.request)),
            Err(e) => Err(end(
                RommScanOutcome::Error,
                task_id,
                stopped(fs_slug, accepted, &e.to_string()),
            )),
        }
    };
    let body = serde_json::json!({"type": "quick", "platform_fs_slugs": [fs_slug]});
    let mut backoff = timing.backoff;
    let task = loop {
        let romm = match client(None) {
            Ok(romm) => romm,
            Err(end) => return end,
        };
        match romm.post::<_, Task>("/api/tasks/scan", &body) {
            Ok(task) => break task,
            Err(RommError::Status { status: 409, .. }) => {
                if Instant::now() + backoff > deadline {
                    return end(
                        RommScanOutcome::TimedOut,
                        None,
                        format!(
                            "RomM was busy with another scan for {}, so {fs_slug} wasn't scanned. Scan it in RomM.",
                            span(timing.window)
                        ),
                    );
                }
                if stop.wait(backoff) {
                    return halted(None);
                }
                backoff = (backoff * 2).min(timing.backoff_max);
            }
            Err(RommError::Status { status: 503, .. }) => {
                return end(
                    RommScanOutcome::Unavailable,
                    None,
                    format!(
                        "RomM has no scan worker running, so {fs_slug} wasn't scanned. Start RomM's worker, then \
                             scan it in RomM."
                    ),
                );
            }
            Err(e) => {
                return end(
                    RommScanOutcome::Error,
                    None,
                    format!("{e}. {fs_slug} wasn't scanned."),
                );
            }
        }
    };
    let id = task.task_id;
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
    {
        return end(
                RommScanOutcome::Error,
                None,
                "RomM accepted the scan, but named its task in a way Title Select can't follow. Check the scan in \
                 RomM."
                    .into(),
            );
    }
    let path = format!("/api/tasks/{id}");
    loop {
        let romm = match client(Some(&id)) {
            Ok(romm) => romm,
            Err(end) => return end,
        };
        match romm.get::<Task>(&path) {
            Ok(task) => match task.status.as_deref() {
                Some("finished") => {
                    return Some(ScanEnd {
                        outcome: RommScanOutcome::Finished,
                        task_id: Some(id),
                        detail: None,
                    });
                }
                Some(status @ ("failed" | "stopped" | "canceled")) => {
                    return end(
                        RommScanOutcome::Failed,
                        Some(&id),
                        format!(
                            "RomM's scan of {fs_slug} ended {status}. Look in RomM for why, and scan it there."
                        ),
                    );
                }
                _ => {}
            },
            Err(RommError::Unreachable(_)) => {}
            Err(e) => {
                return end(
                    RommScanOutcome::Error,
                    Some(&id),
                    format!("{e}. Whether RomM scanned {fs_slug} isn't known; check it in RomM."),
                );
            }
        }
        if Instant::now() + timing.poll > deadline {
            return end(
                RommScanOutcome::TimedOut,
                Some(&id),
                format!(
                    "RomM was still scanning {fs_slug} after {}. It may yet finish; check it in RomM.",
                    span(timing.window)
                ),
            );
        }
        if stop.wait(timing.poll) {
            return halted(Some(&id));
        }
    }
}

/// The RomM scan recorded on a Job, read from the seven columns from `at` that `JOB_QUERY` selects.
pub(crate) fn scan_from_row(row: &rusqlite::Row, at: usize) -> rusqlite::Result<Option<RommScan>> {
    let Some(platform) = row.get::<_, Option<String>>(at)? else {
        return Ok(None);
    };
    let bad = |i: usize, why: String| {
        rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, why.into())
    };
    let platform = serde_json::from_str(&platform).map_err(|e| bad(at, e.to_string()))?;
    let outcome: String = row.get(at + 1)?;
    let outcome = RommScanOutcome::parse(&outcome)
        .ok_or_else(|| bad(at + 1, format!("unknown RomM scan outcome {outcome}")))?;
    Ok(Some(RommScan {
        platform,
        outcome,
        task_id: row.get(at + 2)?,
        detail: row.get(at + 3)?,
        requested_at: row.get(at + 4)?,
        ended_at: row.get(at + 5)?,
        additions: crate::romm_additions::from_column(row, at + 6)?,
    }))
}

/// Why RomM can't scan a changed Library Folder that maps to `platform`, or `None` when it can, or when the folder
/// is none of RomM's business.
fn unscannable(platform: &RommPlatform) -> Option<&'static str> {
    Some(match platform {
        RommPlatform::Mapped { .. } | RommPlatform::OutsideRoot => return None,
        RommPlatform::Nested => {
            "This folder is more than one level under the root RomM mounts as its library, so RomM can't scan it as a \
             platform. Keep each library folder directly under that root."
        }
        RommPlatform::IsRoot => {
            "This folder is the root RomM mounts as its library, so it isn't one RomM platform. Keep each library \
             folder directly under that root."
        }
        RommPlatform::NoRoot => {
            "The root RomM mounts as its library is no longer in your configuration, so RomM wasn't asked to scan. \
             Choose it again in your RomM settings."
        }
        RommPlatform::Unresolved => {
            "This folder or the root RomM mounts as its library can't be found on disk, so RomM wasn't asked to scan."
        }
    })
}

impl Store {
    /// Whether the Build Job `job` changed any file in its Library Folder: put one in place, kept one aside, restored
    /// one, renamed one, or left its target uncertain. Adopting a file where it already is, and forgetting one
    /// already gone, change nothing.
    fn build_changed_files(&self, job: i64) -> Result<bool> {
        let Some((build, plan)) = self.read(|c| {
            Ok(c.query_row(
                "SELECT id, plan_id FROM builds WHERE job_id = ?1",
                [job],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?)
        })?
        else {
            return Ok(false);
        };
        // A file kept aside is recorded before it is moved, and the record dropped if it is left in place, so a
        // record means it left the folder, whatever became of its item: a repack that kept the old copy aside and
        // then failed to put the new one in place still took a file away. A removal that only forgets the record of
        // a file already gone keeps nothing aside, and changes nothing.
        let moved: bool = self.read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM kept_files WHERE build_id = ?1)
                     OR EXISTS (SELECT 1 FROM build_items WHERE build_id = ?1
                                AND state IN ('applied', 'uncertain', 'publishing')
                                AND kind IN ('copy', 'repack', 'restore'))",
                [build],
                |r| r.get(0),
            )?)
        })?;
        if moved {
            return Ok(true);
        }
        let done: Vec<String> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT target FROM build_items
                 WHERE build_id = ?1 AND state IN ('applied', 'uncertain') AND kind = 'adopt'",
            )?;
            let rows = stmt.query_map([build], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        // A cleanup may adopt a file under a new name, which moves it.
        let Some(cleanup) = self.plan(plan)?.content.cleanup else {
            return Ok(false);
        };
        Ok(cleanup.entries.iter().any(|e| {
            e.action == Some(CleanupAction::Adopt)
                && e.target
                    .as_ref()
                    .is_some_and(|target| *target != e.name && done.contains(target))
        }))
    }

    fn record_romm_scan(&self, job: i64, scan: &RommScan) -> Result<Job> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO romm_scans (job_id, platform, outcome, task_id, detail, requested_at, ended_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (job_id) DO UPDATE SET outcome = ?3, task_id = ?4, detail = ?5, ended_at = ?7",
                params![
                    job,
                    serde_json::to_string(&scan.platform).expect("JSON serializes"),
                    scan.outcome.as_str(),
                    scan.task_id,
                    scan.detail,
                    scan.requested_at,
                    scan.ended_at,
                ],
            )?;
            tx.execute(
                "UPDATE jobs SET version = version + 1 WHERE id = ?1",
                [job],
            )?;
            crate::jobs::job_by_id(tx, job)
        })
    }

    /// Requests the RomM scan the ended Build Job `job` owes, if it owes one, and follows it to its end within
    /// `timing` or until `stop`, recording each step on the Job and telling `changed` of the Job as it then is. A
    /// Build, Library cleanup or Kept File restore owes one when RomM is on and it changed files in a Library Folder:
    /// RomM scans a folder directly under its root, and a folder elsewhere under it is recorded as a warning with no
    /// request. No other Job, nor one that changed nothing, owes a scan. Whatever RomM does, the Job's own state stays
    /// as it ended. Returns the Job as its last step left it, or `None` when it owed no scan.
    ///
    /// Nothing is held while it waits on RomM, the Store least of all: it is upgraded from `store` only to read and
    /// record, so a request RomM never answers doesn't keep it open. Once the Store is gone, it records nothing more
    /// and returns `None`. A scan that ends any way but finished after `stop` is recorded as stopped.
    pub fn request_romm_scan(
        store: &Weak<Store>,
        config: &Config,
        job: i64,
        timing: &ScanTiming,
        stop: &ScanStop,
        changed: &mut dyn FnMut(&Job),
    ) -> Result<Option<Job>> {
        let step = |scan: &RommScan, changed: &mut dyn FnMut(&Job)| -> Result<Option<Job>> {
            let Some(store) = store.upgrade() else {
                return Ok(None);
            };
            let job = store.record_romm_scan(job, scan)?;
            changed(&job);
            Ok(Some(job))
        };
        let (romm, fs_slug, mut scan) = {
            let Some(store) = store.upgrade() else {
                return Ok(None);
            };
            let ended = store.job(job)?;
            if ended.kind != JobKind::Build || !ended.state.is_final() || ended.romm_scan.is_some()
            {
                return Ok(None);
            }
            let Some(folder) = ended.library_folder else {
                return Ok(None);
            };
            let romm = store.settings()?.romm;
            if !romm.is_on() || !store.build_changed_files(job)? {
                return Ok(None);
            }
            let (root, path): (String, String) = store.read(|c| {
                Ok(c.query_row(
                    "SELECT root, path FROM library_folders WHERE id = ?1",
                    [folder],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })?;
            let platform = romm.platform(config, &root, &path);
            let mut scan = RommScan {
                platform: platform.clone(),
                outcome: RommScanOutcome::Requested,
                task_id: None,
                detail: None,
                requested_at: now(),
                ended_at: None,
                additions: None,
            };
            let Some(fs_slug) = platform.fs_slug() else {
                let Some(why) = unscannable(&platform) else {
                    return Ok(None);
                };
                scan.outcome = RommScanOutcome::Unmapped;
                scan.detail = Some(why.into());
                scan.ended_at = Some(now());
                drop(store);
                return step(&scan, changed);
            };
            (romm, fs_slug.to_owned(), scan)
        };
        if step(&scan, changed)?.is_none() {
            return Ok(None);
        }
        let Some(mut end) = follow_romm_scan(store, &romm, &fs_slug, timing, stop) else {
            return Ok(None);
        };
        if stop.is_stopped()
            && !matches!(
                end.outcome,
                RommScanOutcome::Finished | RommScanOutcome::Stopped
            )
        {
            end.outcome = RommScanOutcome::Stopped;
            end.detail = Some(stopped(
                &fs_slug,
                true,
                "Title Select stopped while it waited on RomM",
            ));
        }
        scan.outcome = end.outcome;
        scan.task_id = end.task_id;
        scan.detail = end.detail;
        scan.ended_at = Some(now());
        step(&scan, changed)
    }

    /// Records each RomM scan still followed as no longer followed, as Title Select won't follow it again, and
    /// returns their Jobs. Called as the Service stops, and once at start for the scans of a Service that stopped
    /// without it.
    pub fn recover_romm_scans(&self) -> Result<Vec<i64>> {
        self.write(|tx| {
            let jobs: Vec<i64> = {
                let mut stmt = tx.prepare(
                    "UPDATE romm_scans SET outcome = 'stopped', detail = ?1, ended_at = ?2
                     WHERE outcome = 'requested' RETURNING job_id",
                )?;
                let rows = stmt.query_map(
                    params![
                        "Title Select stopped while it followed RomM's scan, so whether it finished isn't known. \
                         Check the platform in RomM.",
                        now()
                    ],
                    |r| r.get(0),
                )?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for job in &jobs {
                tx.execute("UPDATE jobs SET version = version + 1 WHERE id = ?1", [job])?;
            }
            Ok(jobs)
        })
    }

    /// Each Library Folder and the RomM platform it maps to, by the RomM root saved now.
    pub fn romm_platforms(&self, config: &Config) -> Result<Vec<RommFolderPlatform>> {
        let romm = self.settings()?.romm;
        Ok(self
            .library_folders(config)?
            .into_iter()
            .map(|folder| RommFolderPlatform {
                platform: romm.platform(config, &folder.root, &folder.path),
                folder_id: folder.id,
                root: folder.root,
                path: folder.path,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_library_folder_one_level_under_the_romm_root_maps_to_its_fs_slug() {
        let root = Some(Path::new("/library"));
        let mapped = |slug: &str| RommPlatform::Mapped {
            fs_slug: slug.into(),
        };
        for (folder, expected) in [
            ("/library/psx", mapped("psx")),
            ("/library/n64/", mapped("n64")),
            ("/library/./snes", mapped("snes")),
            ("/library/arcade/neogeo", RommPlatform::Nested),
            ("/library", RommPlatform::IsRoot),
            ("/elsewhere/psx", RommPlatform::OutsideRoot),
            ("/library-2/psx", RommPlatform::OutsideRoot),
            ("/library/../psx", RommPlatform::OutsideRoot),
        ] {
            assert_eq!(platform(root, Path::new(folder)), expected, "{folder}");
        }
        assert_eq!(
            platform(None, Path::new("/library/psx")),
            RommPlatform::NoRoot
        );
    }

    /// Roots are compared where they really are on disk, not as their paths are spelled.
    #[cfg(unix)]
    #[test]
    fn a_library_folder_maps_by_where_its_root_and_the_romm_root_really_are() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        for dir in ["mnt/roms/gba", "a/b", "a/psx", "top/psx"] {
            std::fs::create_dir_all(t.join(dir)).unwrap();
        }
        symlink(t.join("mnt/roms"), t.join("library")).unwrap();
        symlink(t.join("a/b"), t.join("top/link")).unwrap();
        let config = Config::parse(&format!(
            "[roots.library]\npath = {:?}\n[roots.mnt]\npath = {:?}\n\
             [roots.top]\npath = {:?}\n[roots.up]\npath = {:?}\n",
            t.join("library").display().to_string(),
            t.join("mnt/roms").display().to_string(),
            t.join("top").display().to_string(),
            t.join("top/link/..").display().to_string(),
        ))
        .unwrap();
        let romm = |root: &str| Romm {
            root: Some(root.into()),
            ..Romm::default()
        };

        assert_eq!(
            romm("library").platform(&config, "mnt", "gba"),
            RommPlatform::Mapped {
                fs_slug: "gba".into()
            },
            "a RomM root spelled through a symlink still holds the folder"
        );
        assert_eq!(
            romm("top").platform(&config, "up", "psx"),
            RommPlatform::OutsideRoot,
            "top/link/.. is really a/, outside top/, however it is spelled"
        );
        assert_eq!(
            romm("library").platform(&config, "mnt", "gone"),
            RommPlatform::Unresolved,
            "a folder missing from disk can't be placed"
        );
    }
}
