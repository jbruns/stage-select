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

    /// This pace's share of a byte-rate ceiling: none under `unthrottled`, which sets no byte cap, nor while there
    /// is no ceiling.
    pub fn rate(self, ceiling: Option<u64>) -> Option<u64> {
        match self {
            Pace::Unthrottled => None,
            _ => ceiling.map(|c| self.of(usize::try_from(c).unwrap_or(usize::MAX)) as u64),
        }
    }

    /// The threads chdman is told to compress with under this pace: its share of the hash workers under `gentle` and
    /// `default`, and none under `unthrottled`, where chdman keeps its own default of every core.
    pub fn chdman_threads(self, hashers: usize) -> Option<usize> {
        match self {
            Pace::Unthrottled => None,
            _ => Some(self.of(hashers)),
        }
    }
}

/// The most worker threads a worker ceiling may be: the most Jobs at once, hash workers or readers on a Root.
pub const MAX_WORKERS: u64 = 1024;
/// The most 7z archives that may decode at once.
pub const MAX_ARCHIVE_WORKERS: u64 = 64;
/// The largest byte-rate ceiling, in MiB/s: 1 TiB/s.
pub const MAX_MIB_PER_S: u64 = 1 << 20;
/// How many 7z archives decode at once unless the Operator says otherwise.
pub const DEFAULT_ARCHIVE_WORKERS: u64 = 2;

/// Where a Ceiling's value comes from: the first of these that has one (ADR 0022).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CeilingSource {
    /// The configuration file, which locks it.
    Config,
    /// The Operator's value, saved in the workbench.
    Yours,
    /// The latest Calibration's knee.
    Calibration,
    /// The CPUs the Service can use.
    Cpus,
    /// A fixed default: 7z archive workers, which aren't measured.
    Default,
}

/// One Ceiling, where it comes from, and what the pace makes of it now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ceiling {
    /// The Ceiling; none for a byte rate nothing sets.
    pub ceiling: Option<u64>,
    /// What the pace allows now: its share of `ceiling`. None for a byte rate under `unthrottled`, or with no
    /// ceiling.
    pub in_force: Option<u64>,
    /// Where `ceiling` comes from; none while it has no value.
    pub source: Option<CeilingSource>,
    /// The Operator's saved value, whether or not it applies.
    pub yours: Option<u64>,
    /// What the latest Calibration measured, where it measures this Ceiling.
    pub measured: Option<u64>,
    /// The configuration key that sets and locks it, as `[roots.sets] readers`; none when the configuration file
    /// doesn't set it.
    pub locked_by: Option<String>,
}

impl Ceiling {
    /// Whether the configuration file sets it, so that the workbench can't.
    pub fn locked(&self) -> bool {
        self.locked_by.is_some()
    }
}

/// The Ceilings of the Host, shared by every Job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCeilings {
    /// Jobs started at once.
    pub concurrent: Ceiling,
    /// Hash workers, one pool for every Job.
    pub hashers: Ceiling,
    /// 7z archives decoded at once.
    pub archive_workers: Ceiling,
}

/// The Ceilings of one Root, shared by every Job reading or writing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootCeilings {
    pub root: String,
    /// Payload readers, across every Job reading the Root.
    pub readers: Ceiling,
    pub read_mib_per_s: Ceiling,
    /// Only ever the Operator's: writes are never measured.
    pub write_mib_per_s: Ceiling,
    /// The fastest write throughput a past Job averaged into this Root, in MiB/s: a starting value for its write
    /// ceiling.
    pub observed_write_mib_per_s: Option<u64>,
}

/// The Operator's own Ceilings, saved in the workbench: each one set overrides the Calibration and the CPU count,
/// but not the configuration file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hashers: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_workers: Option<u64>,
}

/// The Operator's own Ceilings for one Root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readers: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_mib_per_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_mib_per_s: Option<u64>,
}

impl RootOverrides {
    pub fn is_empty(&self) -> bool {
        *self == RootOverrides::default()
    }
}

/// Every Ceiling the Operator set in the workbench.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CeilingOverrides {
    #[serde(default)]
    pub host: HostOverrides,
    /// By Root name; a Root with none set is left out.
    #[serde(default)]
    pub roots: BTreeMap<String, RootOverrides>,
}

/// A Ceiling as `PATCH /settings` takes it: any JSON value, so that one that isn't a whole number in range is
/// refused with the reason, not as a malformed body. Null or absent clears it.
pub type CeilingInput = Option<serde_json::Value>;

/// A change to the Host's Ceilings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostCeilingsUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrent: CeilingInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hashers: CeilingInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_workers: CeilingInput,
}

/// A change to one Root's Ceilings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootCeilingsUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readers: CeilingInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_mib_per_s: CeilingInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_mib_per_s: CeilingInput,
}

/// Every Ceiling the Operator sets in the workbench, replacing those set before: one left out is cleared, and
/// falls back to the Calibration or the CPU count. One the configuration file sets can't be given.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CeilingsUpdate {
    #[serde(default)]
    pub host: HostCeilingsUpdate,
    #[serde(default)]
    pub roots: BTreeMap<String, RootCeilingsUpdate>,
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

/// The Job limits: the pace you chose, and every Ceiling of the Host and of each configured Root, with where it
/// comes from and what the pace allows now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsView {
    pub pace: Pace,
    pub host: HostCeilings,
    pub roots: Vec<RootCeilings>,
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
    /// Replaces every Ceiling you set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceilings: Option<CeilingsUpdate>,
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
        for (ceiling, gentle, default) in [(1, 1, 1), (2, 1, 1), (4, 1, 2), (7, 1, 3), (16, 4, 8)] {
            assert_eq!(Pace::Gentle.of(ceiling), gentle, "{ceiling}");
            assert_eq!(Pace::Default.of(ceiling), default, "{ceiling}");
            assert_eq!(Pace::Unthrottled.of(ceiling), ceiling);
        }
    }

    #[test]
    fn unthrottled_sets_no_byte_cap_and_the_others_a_share_of_the_ceiling() {
        assert_eq!(Pace::Gentle.rate(Some(400)), Some(100));
        assert_eq!(Pace::Default.rate(Some(400)), Some(200));
        assert_eq!(Pace::Default.rate(Some(1)), Some(1));
        assert_eq!(Pace::Unthrottled.rate(Some(400)), None);
        assert_eq!(Pace::Gentle.rate(None), None);
    }

    #[test]
    fn chdman_is_given_the_paces_share_of_the_hash_workers_except_unthrottled() {
        assert_eq!(Pace::Gentle.chdman_threads(16), Some(4));
        assert_eq!(Pace::Default.chdman_threads(16), Some(8));
        assert_eq!(Pace::Gentle.chdman_threads(2), Some(1));
        assert_eq!(Pace::Unthrottled.chdman_threads(16), None);
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
