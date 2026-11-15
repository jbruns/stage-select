//! RomM, an optional integration that is off by default (ADR 0020): its connection as stored, a small client for its
//! API, testing the connection, and the RomM platform each Library Folder maps to.

use std::path::{Component, Path};

use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use title_select_api::romm::{
    RommCheck, RommFolderPlatform, RommPlatform, RommTestRequest, RommUpdate, RommUser, RommView,
    version_tested,
};
use title_select_api::settings::SecretUpdate;

use crate::config::Config;
use crate::fetch::{Http, Method, refusal_unnamed};
use crate::{Error, Result, Store};

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
        let url = format!("{}{path}", self.base_url);
        let bearer = self.token.as_ref().map(|t| format!("Bearer {t}"));
        let mut headers = vec![("Accept", "application/json")];
        if let Some(bearer) = &bearer {
            headers.push(("Authorization", bearer));
        }
        let body = body.map(|b| serde_json::to_vec(b).expect("JSON serializes"));
        if body.is_some() {
            headers.push(("Content-Type", "application/json"));
        }
        let (status, bytes) = self
            .http
            .send(
                method,
                &url,
                &headers,
                body.as_deref(),
                self.timeout,
                self.limit,
            )
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

    fn read<T: DeserializeOwned>(path: &str, bytes: &[u8]) -> std::result::Result<T, RommError> {
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
            for (scope, method, path) in PROBES {
                let body = (method == Method::Post).then(|| serde_json::json!({"rom_ids": []}));
                match self.send(method, path, body.as_ref()) {
                    Ok(_) | Err(RommError::Status { status: 404, .. }) => {}
                    Err(RommError::Forbidden { .. }) => check.missing_scopes.push(scope.into()),
                    Err(e) => return Err(e),
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

impl Store {
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
