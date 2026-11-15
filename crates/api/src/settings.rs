//! The Operator's operational settings, edited in the workbench and kept in the store. Each section is stored and
//! replaced on its own, so a later section (such as RomM's connection) is one more field here. Secrets are
//! write-only: [`SettingsView`] says only whether one is set, and [`SecretUpdate`] keeps, replaces or clears it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Workers;
use crate::paths::{DEFAULT_LIBRARY_FOLDER_TEMPLATE, DEFAULT_SOURCE_SET_TEMPLATE, PathTemplate};

/// How hard Jobs work the disks and CPUs: a fraction of the configured ceilings, applied to running Jobs at each
/// worker's next file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pace {
    /// A quarter of each ceiling: for a disk shared with other work.
    Gentle,
    /// Half of each ceiling.
    #[default]
    Default,
    /// Every ceiling in full. An explicit choice, marked while active.
    Unthrottled,
}

impl Pace {
    pub const ALL: [Pace; 3] = [Pace::Gentle, Pace::Default, Pace::Unthrottled];

    pub fn as_str(self) -> &'static str {
        match self {
            Pace::Gentle => "gentle",
            Pace::Default => "default",
            Pace::Unthrottled => "unthrottled",
        }
    }

    pub fn parse(s: &str) -> Option<Pace> {
        Pace::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// This pace's share of `ceiling`, rounded down, and never less than 1.
    pub fn of(self, ceiling: usize) -> usize {
        let share = match self {
            Pace::Gentle => ceiling / 4,
            Pace::Default => ceiling / 2,
            Pace::Unthrottled => ceiling,
        };
        share.max(1)
    }

    /// This pace's share of each worker ceiling.
    pub fn workers(self, ceilings: Workers) -> Workers {
        Workers {
            readers: self.of(ceilings.readers),
            hashers: self.of(ceilings.hashers),
        }
    }

    /// This pace's share of each ceiling.
    pub fn counts(self, ceilings: Ceilings) -> Ceilings {
        Ceilings {
            concurrent: self.of(ceilings.concurrent),
            readers: self.of(ceilings.readers),
            hashers: self.of(ceilings.hashers),
        }
    }
}

/// The most Jobs that run at once, and the most payload readers and hash workers one Job runs, from the
/// configuration file. The pace scales each down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ceilings {
    pub concurrent: usize,
    pub readers: usize,
    pub hashers: usize,
}

impl Ceilings {
    pub fn workers(self) -> Workers {
        Workers {
            readers: self.readers,
            hashers: self.hashers,
        }
    }
}

/// How many full Audits and Selections each Source Set keeps; older ones are pruned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    pub audits_keep: usize,
    pub selections_keep: usize,
}

/// The most full Audits or Selections a Source Set may keep.
pub const MAX_KEPT: usize = 10_000;
/// The longest a check-for-changes interval may be, in minutes: a year.
pub const MAX_CHECK_EVERY_MINUTES: u64 = 525_600;
/// The longest chdman's time limit may be, in seconds, before or for each GiB: a week.
pub const MAX_CHD_SECONDS: u64 = 604_800;
/// The longest an Ingest Folder's settle time may be, in seconds: a day.
pub const MAX_SETTLE_SECS: u64 = 86_400;
/// The longest an Ingest Folder's scan schedule may be, in minutes: a week.
pub const MAX_SCAN_EVERY_MINUTES: u64 = 10_080;

/// How many full Audits a Source Set keeps unless the Operator says otherwise.
pub const DEFAULT_AUDITS_KEPT: usize = 20;
/// How many full Selections a Source Set keeps unless the Operator says otherwise.
pub const DEFAULT_SELECTIONS_KEPT: usize = 20;

impl Default for Retention {
    fn default() -> Self {
        Retention {
            audits_keep: DEFAULT_AUDITS_KEPT,
            selections_keep: DEFAULT_SELECTIONS_KEPT,
        }
    }
}

/// The default Path Templates that propose a registration's path. Each can be overridden at registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathTemplates {
    /// For a Source Set, relative to its Root.
    pub source_set: PathTemplate,
    /// For a Library Folder, relative to its Play Library Root.
    pub library_folder: PathTemplate,
}

impl Default for PathTemplates {
    fn default() -> Self {
        PathTemplates {
            source_set: PathTemplate::parse(DEFAULT_SOURCE_SET_TEMPLATE)
                .expect("the default template parses"),
            library_folder: PathTemplate::parse(DEFAULT_LIBRARY_FOLDER_TEMPLATE)
                .expect("the default template parses"),
        }
    }
}

/// How often each audited Source Set is checked for changes unless the Operator says otherwise: every six hours.
pub const DEFAULT_CHECK_EVERY_MINUTES: u64 = 360;

/// How often a stat walk checks each audited Source Set for files changed since its latest Audit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSetChecks {
    /// In minutes; 0 checks only when asked.
    pub check_every_minutes: u64,
}

impl Default for SourceSetChecks {
    fn default() -> Self {
        SourceSetChecks {
            check_every_minutes: DEFAULT_CHECK_EVERY_MINUTES,
        }
    }
}

/// Which DAT Providers Title Select fetches newer DAT Revisions from (ADR 0012). Each is off until turned on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchSettings {
    pub redump: bool,
    pub mame: bool,
}

impl FetchSettings {
    pub fn enabled(&self, provider: crate::fetch::FetchProvider) -> bool {
        match provider {
            crate::fetch::FetchProvider::Redump => self.redump,
            crate::fetch::FetchProvider::Mame => self.mame,
        }
    }
}

/// How long chdman may run on a disc before it is stopped and the disc fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChdTimeouts {
    /// Seconds for each run, before `seconds_per_gib` is added.
    pub seconds: u64,
    /// Seconds added for each GiB of the disc's tracks.
    pub seconds_per_gib: u64,
}

impl Default for ChdTimeouts {
    /// Generous, so that only a stuck chdman reaches them: chdman 0.289 compressed a GiB of incompressible CD tracks
    /// in about a minute on one fast desktop core, its worst case. 4 minutes a GiB allows a host core four times
    /// slower; 10 minutes covers starting up, and a small disc on a busy host.
    fn default() -> Self {
        ChdTimeouts {
            seconds: 600,
            seconds_per_gib: 240,
        }
    }
}

/// How long an item in an Ingest Folder must stay unchanged before it is assessed, unless the Operator says
/// otherwise.
pub const DEFAULT_SETTLE_SECS: u64 = 60;

/// How one Ingest Folder is watched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IngestCadence {
    /// How long, in seconds, an item's size and times must stay unchanged before it is assessed.
    pub settle_secs: u64,
    /// How often, in minutes, it is scanned on its own; never when absent.
    #[serde(default)]
    pub scan_every_minutes: Option<u64>,
}

impl Default for IngestCadence {
    fn default() -> Self {
        IngestCadence {
            settle_secs: DEFAULT_SETTLE_SECS,
            scan_every_minutes: None,
        }
    }
}

/// An event the Service can post to the webhook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum NotifyEvent {
    #[serde(rename = "job.finished")]
    JobFinished,
    #[serde(rename = "job.failed")]
    JobFailed,
    #[serde(rename = "source_set.stale")]
    SourceSetStale,
    #[serde(rename = "dat_revision.new")]
    DatRevisionNew,
    #[serde(rename = "fetch.failed")]
    FetchFailed,
    #[serde(rename = "drift.found")]
    DriftFound,
}

impl NotifyEvent {
    pub const ALL: [NotifyEvent; 6] = [
        NotifyEvent::JobFinished,
        NotifyEvent::JobFailed,
        NotifyEvent::SourceSetStale,
        NotifyEvent::DatRevisionNew,
        NotifyEvent::FetchFailed,
        NotifyEvent::DriftFound,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            NotifyEvent::JobFinished => "job.finished",
            NotifyEvent::JobFailed => "job.failed",
            NotifyEvent::SourceSetStale => "source_set.stale",
            NotifyEvent::DatRevisionNew => "dat_revision.new",
            NotifyEvent::FetchFailed => "fetch.failed",
            NotifyEvent::DriftFound => "drift.found",
        }
    }
}

fn all_notify_events() -> Vec<NotifyEvent> {
    NotifyEvent::ALL.to_vec()
}

/// The webhook, without its secrets: whether its URL and signing secret are set, which events it is sent, and the
/// workbench URL its links start with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyView {
    pub url_set: bool,
    pub secret_set: bool,
    pub events: Vec<NotifyEvent>,
    pub workbench_url: Option<String>,
}

/// What to do with a write-only secret: `"keep"`, `"clear"` or `{"set": "…"}`. Neither a refusal to read one nor
/// its `Debug` shows the secret.
#[derive(Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretUpdate {
    /// Leave it as it is.
    #[default]
    Keep,
    /// Replace it with this.
    Set(String),
    /// Remove it.
    Clear,
}

impl std::fmt::Debug for SecretUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SecretUpdate::Keep => "Keep",
            SecretUpdate::Set(_) => "Set(..)",
            SecretUpdate::Clear => "Clear",
        })
    }
}

impl<'de> Deserialize<'de> for SecretUpdate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        const EXPECTED: &str = r#""keep", "clear" or {"set": "…"}"#;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = SecretUpdate;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str(EXPECTED)
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<SecretUpdate, E> {
                match v {
                    "keep" => Ok(SecretUpdate::Keep),
                    "clear" => Ok(SecretUpdate::Clear),
                    _ => Err(E::custom(format!("expected {EXPECTED}"))),
                }
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<SecretUpdate, A::Error> {
                let refused = || serde::de::Error::custom(format!("expected {EXPECTED}"));
                let Some(key) = map.next_key::<String>()? else {
                    return Err(refused());
                };
                if key != "set" {
                    return Err(refused());
                }
                let secret = map.next_value::<String>().map_err(|_| refused())?;
                if map.next_key::<String>()?.is_some() {
                    return Err(refused());
                }
                Ok(SecretUpdate::Set(secret))
            }
        }
        d.deserialize_any(Visitor)
    }
}

/// A change to the webhook. The URL and secret are write-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifyUpdate {
    #[serde(default)]
    pub url: SecretUpdate,
    #[serde(default)]
    pub secret: SecretUpdate,
    #[serde(default = "all_notify_events")]
    pub events: Vec<NotifyEvent>,
    #[serde(default)]
    pub workbench_url: Option<String>,
}

/// The Job limits: the pace you chose, the ceilings from the configuration file, and the counts the pace gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsView {
    pub pace: Pace,
    pub ceilings: Ceilings,
    pub active: Ceilings,
}

/// Every operational setting, as `GET /settings` returns it: never a secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsView {
    pub jobs: JobsView,
    pub retention: Retention,
    pub path_templates: PathTemplates,
    pub source_sets: SourceSetChecks,
    pub fetch: FetchSettings,
    pub chd: ChdTimeouts,
    /// Each Ingest Folder's cadence, by its Root's name: every ingest Root, set or not.
    pub ingest_folders: BTreeMap<String, IngestCadence>,
    pub notify: NotifyView,
    pub romm: crate::romm::RommView,
}

/// A change to some settings, as `PATCH /settings` takes it: each section given replaces that section, and the rest
/// stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pace: Option<Pace>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<Retention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_templates: Option<PathTemplates>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sets: Option<SourceSetChecks>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<FetchSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chd: Option<ChdTimeouts>,
    /// The cadence of each Ingest Folder named; those not named stay as they are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingest_folders: Option<BTreeMap<String, IngestCadence>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<NotifyUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub romm: Option<crate::romm::RommUpdate>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pace_is_a_fraction_of_each_ceiling_rounded_down_and_at_least_one() {
        let ceilings = Ceilings {
            concurrent: 3,
            readers: 8,
            hashers: 1,
        };
        assert_eq!(
            Pace::Gentle.counts(ceilings),
            Ceilings {
                concurrent: 1,
                readers: 2,
                hashers: 1
            }
        );
        assert_eq!(
            Pace::Default.counts(ceilings),
            Ceilings {
                concurrent: 1,
                readers: 4,
                hashers: 1
            }
        );
        assert_eq!(Pace::Unthrottled.counts(ceilings), ceilings);
        for (ceiling, gentle, default) in [(1, 1, 1), (2, 1, 1), (4, 1, 2), (7, 1, 3), (16, 4, 8)] {
            assert_eq!(Pace::Gentle.of(ceiling), gentle, "{ceiling}");
            assert_eq!(Pace::Default.of(ceiling), default, "{ceiling}");
            assert_eq!(Pace::Unthrottled.of(ceiling), ceiling);
        }
    }

    #[test]
    fn a_secret_update_keeps_unless_told_otherwise() {
        let update: NotifyUpdate = serde_json::from_str(r#"{"secret": {"set": "s"}}"#).unwrap();
        assert_eq!(update.url, SecretUpdate::Keep);
        assert_eq!(update.secret, SecretUpdate::Set("s".into()));
        assert_eq!(update.events, NotifyEvent::ALL);
        let clear: NotifyUpdate = serde_json::from_str(r#"{"url": "clear"}"#).unwrap();
        assert_eq!(clear.url, SecretUpdate::Clear);
    }

    #[test]
    fn a_secret_is_never_echoed_by_a_refusal_or_a_debug_print() {
        for body in [
            r#"{"url": "https://hooks.example/secret-token"}"#,
            r#"{"url": {"put": "secret-token"}}"#,
            r#"{"url": {"set": "x", "secret-token": 1}}"#,
            r#"{"url": ["secret-token"]}"#,
        ] {
            let err = serde_json::from_str::<NotifyUpdate>(body)
                .unwrap_err()
                .to_string();
            assert!(!err.contains("secret-token"), "{err}");
        }
        let set = SecretUpdate::Set("secret-token".into());
        assert!(!format!("{set:?}").contains("secret-token"));
        assert_eq!(
            serde_json::to_string(&set).unwrap(),
            r#"{"set":"secret-token"}"#
        );
    }
}
