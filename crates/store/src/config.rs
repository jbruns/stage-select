//! The Operator's configuration: `title-select.toml` (ADR 0006). It holds only what bootstraps the Service until
//! #266 moves it to the environment: auth and chdman. Roots, Ceilings and archive limits live in the store
//! ([`crate::roots`], [`crate::settings`]), edited in the workbench, the CLI or the API.

use std::path::{Path, PathBuf};

use ipnet::IpNet;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub chd: Chd,
}

/// How a Build makes a CHD.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chd {
    /// The chdman to run. When absent, the program `TITLE_SELECT_CHDMAN` names, else `chdman` on the `PATH`.
    pub chdman: Option<PathBuf>,
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
        Ok(config)
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
    fn a_config_that_does_not_parse_is_named_by_line_and_reason_never_quoted() {
        let err =
            Config::parse("[chd]\nchdman = \"/bin/x\"\n\n[auth]\ndev_identity = \"secret-token\n")
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
    fn settings_kept_in_the_store_are_not_read_from_the_file() {
        for text in [
            "[roots.sets]\npath = \"/sets\"",
            "[jobs]\nconcurrent = 2",
            "[ingest]\nmax_dictionary_mib = 64",
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
