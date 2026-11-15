//! The Operator's configuration: `title-select.toml` (ADR 0006). It holds only what bootstraps the Service and bounds
//! what it may do: Roots, auth, chdman, Job ceilings and archive limits. Operational settings live in the store
//! ([`crate::settings`]), edited in the workbench.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ipnet::IpNet;
use serde::Deserialize;
use title_select_api::paths::relative_path;
use title_select_api::settings::Ceilings;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Named Roots: the folders Title Select may read.
    #[serde(default)]
    pub roots: BTreeMap<String, Root>,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub jobs: Jobs,
    #[serde(default)]
    pub chd: Chd,
    #[serde(default)]
    pub ingest: Ingest,
}

/// How archives arriving in an Ingest Folder are read: how many 7z archives decode at once across every Job, and
/// the largest dictionary one may declare. Decoding takes up to about `archive_workers` x `max_dictionary_mib` of
/// memory above the rest of the Service.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Ingest {
    pub archive_workers: usize,
    pub max_dictionary_mib: u32,
}

impl Ingest {
    /// The most 7z archives that may decode at once.
    pub const MAX_ARCHIVE_WORKERS: usize = 64;
    /// The largest dictionary limit: 4 GiB, the most an LZMA2 header can declare.
    pub const MAX_DICTIONARY_MIB: u32 = 4096;
}

impl Default for Ingest {
    fn default() -> Self {
        Ingest {
            archive_workers: 2,
            max_dictionary_mib: 128,
        }
    }
}

/// How a Build makes a CHD.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chd {
    /// The chdman to run. When absent, the program `TITLE_SELECT_CHDMAN` names, else `chdman` on the `PATH`.
    pub chdman: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    pub path: PathBuf,
    /// Whether Title Select may write to this Root: Builds to its Library Folders, and repairs to any Source Set
    /// in it. A Source Set is registered only in a read-only Root.
    #[serde(default)]
    pub writable: bool,
    /// For a read-only Root: the paths, relative to it, of the Source Sets in it that a repair may write to.
    #[serde(default)]
    pub writable_paths: Vec<String>,
    /// What the Root is for: `ingest` makes it an Ingest Folder, which must be writable and holds no Source Set or
    /// Library Folder.
    #[serde(default)]
    pub kind: RootKind,
}

/// The Root that holds the Additions Sets, one folder per system.
pub const ADDITIONS_ROOT: &str = "additions";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootKind {
    #[default]
    General,
    /// An Ingest Folder: where new files arrive, assessed, and moved out only by plans you approve.
    Ingest,
}

impl Root {
    pub fn is_ingest(&self) -> bool {
        self.kind == RootKind::Ingest
    }

    /// Whether Title Select may write to the folder `rel` of this Root: the Root is writable, or lists `rel` among
    /// its `writable_paths`.
    pub fn may_write(&self, rel: &str) -> bool {
        self.writable
            || relative_path(rel).is_ok_and(|rel| {
                self.writable_paths
                    .iter()
                    .any(|p| relative_path(p).is_ok_and(|p| p == rel))
            })
    }
}

/// How the Service knows a request comes from the Operator. The Operator token, read from the file named by
/// `TITLE_SELECT_OPERATOR_TOKEN_FILE`, is the other way.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Auth {
    /// The request header carrying the Operator's identity, set by an authenticating reverse proxy.
    pub identity_header: Option<String>,
    /// The only peers, as CIDRs, whose `identity_header` is trusted.
    #[serde(default)]
    pub trusted_proxies: Vec<IpNet>,
    /// An identity given to every request with no other credential, for development. Only allowed while the
    /// Service listens on a loopback address.
    pub dev_identity: Option<String>,
}

/// The most the Service may work at once: Jobs running, and each Job's payload readers and hash workers. Each
/// defaults to the number of CPUs; the pace chosen in the workbench is a fraction of them.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Jobs {
    pub concurrent: Option<usize>,
    pub readers: Option<usize>,
    pub hashers: Option<usize>,
}

impl Jobs {
    /// Each ceiling, as configured or else the number of CPUs.
    pub fn ceilings(&self) -> Ceilings {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        Ceilings {
            concurrent: self.concurrent.unwrap_or(cpus),
            readers: self.readers.unwrap_or(cpus),
            hashers: self.hashers.unwrap_or(cpus),
        }
    }
}

impl Default for Config {
    /// No Roots, no auth, Job ceilings from the CPUs, chdman found as usual and the default archive limits.
    fn default() -> Self {
        Config {
            roots: BTreeMap::new(),
            auth: Auth::default(),
            jobs: Jobs::default(),
            chd: Chd::default(),
            ingest: Ingest::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid configuration: {0}")]
pub struct Error(String);

/// Where `text` fails to parse, and why, never quoting it: any line may hold a secret.
fn parse_error(text: &str, e: &toml::de::Error) -> Error {
    let reason = e.message().trim();
    let Some(span) = e.span() else {
        return Error(reason.to_owned());
    };
    let before = &text[..span.start.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    // The key the line sets, if it is a plain one, but never its value.
    let start = before.rfind('\n').map_or(0, |i| i + 1);
    let key = text[start..]
        .split_once('=')
        .map(|(key, _)| key.trim())
        .filter(|key| {
            !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        });
    match key {
        Some(key) => Error(format!("line {line}, column {column}, `{key}`: {reason}")),
        None => Error(format!("line {line}, column {column}: {reason}")),
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, Error> {
        let text =
            std::fs::read_to_string(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
        Config::parse(&text).map_err(|Error(e)| Error(format!("{}: {e}", path.display())))
    }

    pub fn parse(text: &str) -> Result<Config, Error> {
        let config: Config = toml::from_str(text).map_err(|e| parse_error(text, &e))?;
        let jobs = &config.jobs;
        if [jobs.concurrent, jobs.readers, jobs.hashers].contains(&Some(0)) {
            return Err(Error(
                "jobs.concurrent, jobs.readers and jobs.hashers must be at least 1".into(),
            ));
        }
        if !(1..=Ingest::MAX_ARCHIVE_WORKERS).contains(&config.ingest.archive_workers) {
            return Err(Error(format!(
                "ingest.archive_workers must be between 1 and {}",
                Ingest::MAX_ARCHIVE_WORKERS
            )));
        }
        if !(1..=Ingest::MAX_DICTIONARY_MIB).contains(&config.ingest.max_dictionary_mib) {
            return Err(Error(format!(
                "ingest.max_dictionary_mib must be between 1 and {}",
                Ingest::MAX_DICTIONARY_MIB
            )));
        }
        let auth = &config.auth;
        match (&auth.identity_header, auth.trusted_proxies.is_empty()) {
            (Some(header), false) if is_header_name(header) => {}
            (Some(header), false) => {
                return Err(Error(format!(
                    "auth.identity_header {header:?} is not a header name"
                )));
            }
            (Some(_), true) => {
                return Err(Error(
                    "auth.identity_header needs auth.trusted_proxies".into(),
                ));
            }
            (None, false) => {
                return Err(Error(
                    "auth.trusted_proxies needs auth.identity_header".into(),
                ));
            }
            (None, true) => {}
        }
        if auth
            .dev_identity
            .as_deref()
            .is_some_and(|i| i.trim().is_empty())
        {
            return Err(Error("auth.dev_identity is empty".into()));
        }
        for (name, root) in &config.roots {
            if name.is_empty() {
                return Err(Error("a root has an empty name".into()));
            }
            if root.is_ingest() && !root.writable {
                return Err(Error(format!(
                    "roots.{name} is an ingest folder, which needs writable = true"
                )));
            }
            if !root.writable_paths.is_empty() && (root.writable || root.is_ingest()) {
                return Err(Error(format!(
                    "roots.{name}.writable_paths is only for a read-only root; this one is writable already"
                )));
            }
            for path in &root.writable_paths {
                if let Err(e) = relative_path(path) {
                    return Err(Error(format!(
                        "roots.{name}.writable_paths: {path:?}: a source set's path {e}"
                    )));
                }
            }
            if name == ADDITIONS_ROOT
                && (!root.writable || root.is_ingest() || !root.writable_paths.is_empty())
            {
                return Err(Error(format!(
                    "roots.{name} holds the additions sets, which needs writable = true, no kind and no \
                     writable_paths"
                )));
            }
        }
        Ok(config)
    }

    pub fn root(&self, name: &str) -> Option<&Root> {
        self.roots.get(name)
    }
}

/// Whether `name` is a valid HTTP header name: a non-empty token.
fn is_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_archive_limits_default_to_two_workers_and_128_mib_and_refuse_nonsense() {
        let ingest = Config::default().ingest;
        assert_eq!(
            (ingest.archive_workers, ingest.max_dictionary_mib),
            (2, 128)
        );
        let set =
            Config::parse("[ingest]\narchive_workers = 4\nmax_dictionary_mib = 1536").unwrap();
        assert_eq!(
            (set.ingest.archive_workers, set.ingest.max_dictionary_mib),
            (4, 1536)
        );
        for (bad, why) in [
            (
                "archive_workers = 0",
                "ingest.archive_workers must be between 1 and 64",
            ),
            (
                "archive_workers = 1000",
                "ingest.archive_workers must be between 1 and 64",
            ),
            (
                "max_dictionary_mib = 0",
                "ingest.max_dictionary_mib must be between 1 and 4096",
            ),
            (
                "max_dictionary_mib = 1000000",
                "ingest.max_dictionary_mib must be between 1 and 4096",
            ),
        ] {
            let err = Config::parse(&format!("[ingest]\n{bad}"))
                .unwrap_err()
                .to_string();
            assert!(err.contains(why), "{bad}: {err}");
        }
        for bad in [
            "archive_workers = -1",
            "max_dictionary_mib = \"lots\"",
            "workers = 2",
        ] {
            assert!(Config::parse(&format!("[ingest]\n{bad}")).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_config_that_does_not_parse_is_named_by_line_and_reason_never_quoted() {
        let err =
            Config::parse("[jobs]\nconcurrent = 2\n\n[auth]\ndev_identity = \"secret-token\n")
                .unwrap_err()
                .to_string();
        assert!(err.contains("line 5, column 29, `dev_identity`: "), "{err}");
        assert!(err.contains("string"), "{err}");
        assert!(!err.contains("secret-token"), "{err}");
        let err = Config::parse("[auth]\ndev_identity = \"secret-token\"\nx = 1")
            .unwrap_err()
            .to_string();
        assert!(err.contains("line 3, column 1"), "{err}");
        assert!(err.contains("unknown field `x`"), "{err}");
        assert!(!err.contains("secret-token"), "{err}");
    }

    #[test]
    fn job_ceilings_default_to_the_cpus_and_zero_is_refused() {
        let cpus = std::thread::available_parallelism().unwrap().get();
        assert_eq!(
            Config::default().jobs.ceilings(),
            Ceilings {
                concurrent: cpus,
                readers: cpus,
                hashers: cpus
            }
        );
        let set = Config::parse("[jobs]\nconcurrent = 2\nhashers = 6").unwrap();
        assert_eq!(
            set.jobs.ceilings(),
            Ceilings {
                concurrent: 2,
                readers: cpus,
                hashers: 6
            }
        );
        for text in [
            "[jobs]\nconcurrent = 0",
            "[jobs]\nreaders = 0",
            "[jobs]\nhashers = 0",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn settings_edited_in_the_workbench_are_not_read_from_the_file() {
        for text in [
            "[jobs]\npace = \"gentle\"",
            "[audits]\nkeep = 3",
            "[selections]\nkeep = 3",
            "[path_templates]\nsource_set = \"{dat}\"",
            "[source_sets]\ncheck_every_minutes = 5",
            "[fetch]\nredump = true",
            "[notify]\nurl = \"https://hooks.example/x\"",
            "[chd]\ntimeout_seconds = 5",
            "[roots.in]\npath = \"/in\"\nwritable = true\nkind = \"ingest\"\nsettle_secs = 5",
            "[roots.in]\npath = \"/in\"\nwritable = true\nkind = \"ingest\"\nscan_every_minutes = 5",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn an_ingest_root_is_writable() {
        let config =
            Config::parse("[roots.in]\npath = \"/in\"\nwritable = true\nkind = \"ingest\"")
                .unwrap();
        assert!(config.root("in").unwrap().is_ingest());

        for text in [
            "[roots.in]\npath = \"/in\"\nkind = \"ingest\"",
            "[roots.in]\npath = \"/in\"\nkind = \"other\"",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn writable_paths_let_a_read_only_root_write_only_those_folders() {
        let config = Config::parse(
            "[roots.sets]\npath = \"/sets\"\nwritable_paths = [\"No-Intro/Atari 2600\", \"Other/\"]",
        )
        .unwrap();
        let root = config.root("sets").unwrap();
        assert!(!root.writable);
        assert!(root.may_write("No-Intro/Atari 2600"));
        assert!(root.may_write("Other"));
        assert!(!root.may_write("No-Intro"));
        assert!(!root.may_write("No-Intro/Atari 2600/sub"));

        for text in [
            "[roots.sets]\npath = \"/sets\"\nwritable = true\nwritable_paths = [\"a\"]",
            "[roots.sets]\npath = \"/sets\"\nwritable_paths = [\"../a\"]",
            "[roots.sets]\npath = \"/sets\"\nwritable_paths = [\"/a\"]",
            "[roots.in]\npath = \"/in\"\nwritable = true\nkind = \"ingest\"\nwritable_paths = [\"a\"]",
        ] {
            assert!(Config::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn an_identity_header_and_trusted_proxies_need_each_other() {
        let both = "[auth]\nidentity_header = \"Remote-User\"\ntrusted_proxies = [\"10.0.0.0/8\", \"::1/128\"]";
        let config = Config::parse(both).unwrap();
        assert_eq!(config.auth.trusted_proxies.len(), 2);

        assert!(Config::parse("[auth]\nidentity_header = \"Remote-User\"").is_err());
        assert!(Config::parse("[auth]\ntrusted_proxies = [\"10.0.0.0/8\"]").is_err());
        assert!(
            Config::parse(
                "[auth]\nidentity_header = \"Remote User\"\ntrusted_proxies = [\"10.0.0.0/8\"]"
            )
            .is_err()
        );
        assert!(
            Config::parse(
                "[auth]\nidentity_header = \"Remote-User\"\ntrusted_proxies = [\"nonsense\"]"
            )
            .is_err()
        );
    }
}
