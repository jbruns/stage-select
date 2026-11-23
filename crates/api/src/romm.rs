//! RomM, an optional integration that is off by default (ADR 0020): its connection settings, the result of testing
//! the connection, the RomM platform each Library Folder maps to, the scan a Build requests, and the comparison of
//! RomM's view of a Library Folder with what its Builds made.

use serde::{Deserialize, Serialize};

use crate::settings::SecretUpdate;

/// The scopes a RomM Client API Token needs (research 0136 §2).
pub const REQUIRED_SCOPES: [&str; 5] = [
    "roms.read",
    "platforms.read",
    "collections.read",
    "collections.write",
    "tasks.run",
];

/// The RomM releases Stage Select is tested against, as `major.minor`.
pub const TESTED_RELEASES: &str = "5.4";

/// Whether a RomM version is in the tested range ([`TESTED_RELEASES`]): `5.4`, `5.4.1` or `v5.4.2` are, while
/// `5.5.0-alpha.1`, `5.40.0` and `development` aren't.
pub fn version_tested(version: &str) -> bool {
    let version = version.trim().trim_start_matches('v');
    let Some(rest) = version.strip_prefix(TESTED_RELEASES) else {
        return false;
    };
    match rest.strip_prefix('.') {
        None => rest.is_empty(),
        Some(patch) => !patch.is_empty() && patch.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// The RomM connection, without its token: whether it is on, RomM's address, whether a token is set, and which Root
/// RomM mounts as its library (`roms/`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommView {
    pub enabled: bool,
    pub base_url: Option<String>,
    pub token_set: bool,
    pub root: Option<String>,
}

/// A change to the RomM connection, given whole. The token is write-only, and must be given again when the address
/// changes, so that a saved token is never sent to an address it wasn't saved with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RommUpdate {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub token: SecretUpdate,
    #[serde(default)]
    pub root: Option<String>,
}

/// What `POST /romm/test` tests: the saved connection, or the address and token given in its place. A saved token is
/// tested only at its saved address.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RommTestRequest {
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub token: SecretUpdate,
}

/// The RomM user a token belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommUser {
    pub username: String,
    pub admin: bool,
}

/// What testing the RomM connection found. A check that stopped part way says why in `failure`, and keeps what it
/// had found by then.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommCheck {
    /// The version RomM's heartbeat reports.
    pub version: Option<String>,
    /// Whether that version is one Stage Select is tested against.
    pub version_tested: bool,
    /// Who the token belongs to; absent unless the token may read its own profile (`me.read`).
    pub user: Option<RommUser>,
    /// Each of [`REQUIRED_SCOPES`] the token lacks, in that order.
    pub missing_scopes: Vec<String>,
    /// Why the check stopped, if it did.
    pub failure: Option<String>,
}

/// The RomM platform a Library Folder maps to, or why it maps to none. RomM scans a folder exactly one level under
/// the Root it mounts at `roms/`, and names its platform by that folder's name, its `fs_slug`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mapping", rename_all = "snake_case")]
pub enum RommPlatform {
    /// The folder is `<root>/<fs_slug>`.
    Mapped { fs_slug: String },
    /// No RomM root is set, or that Root is no longer registered.
    NoRoot,
    /// The folder isn't under the RomM root.
    OutsideRoot,
    /// The folder is under the RomM root, but more than one level down.
    Nested,
    /// The folder is the RomM root itself.
    IsRoot,
    /// The Library Folder or the RomM root can't be found on disk, so where it really is isn't known.
    Unresolved,
}

impl RommPlatform {
    pub fn fs_slug(&self) -> Option<&str> {
        match self {
            RommPlatform::Mapped { fs_slug } => Some(fs_slug),
            _ => None,
        }
    }
}

/// How a RomM scan requested after a Build Job ended, or stands now. Anything but `requested` and `finished` is a
/// warning on the Job, never its failure (ADR 0020).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RommScanOutcome {
    /// Asked for, and not yet seen to end.
    Requested,
    /// RomM's task finished.
    Finished,
    /// Stage Select stopped following the scan before it ended: the Service stopped, or RomM was turned off or its
    /// address or root changed meanwhile.
    Stopped,
    /// RomM's task failed, or was stopped or cancelled.
    Failed,
    /// RomM was still scanning, or still busy with another scan, when Stage Select stopped waiting.
    TimedOut,
    /// RomM has no scan worker running (HTTP 503).
    Unavailable,
    /// The request or its follow-up was refused or went unanswered: RomM unreachable, the token refused or lacking a
    /// scope, or an answer that doesn't read.
    Error,
    /// The Library Folder changed, but RomM can't scan it on its own: it is nested deeper than one level under the
    /// RomM root, is the RomM root itself, or it or the RomM root can't be found.
    Unmapped,
}

impl RommScanOutcome {
    pub const ALL: [RommScanOutcome; 8] = [
        RommScanOutcome::Requested,
        RommScanOutcome::Finished,
        RommScanOutcome::Stopped,
        RommScanOutcome::Failed,
        RommScanOutcome::TimedOut,
        RommScanOutcome::Unavailable,
        RommScanOutcome::Error,
        RommScanOutcome::Unmapped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RommScanOutcome::Requested => "requested",
            RommScanOutcome::Finished => "finished",
            RommScanOutcome::Stopped => "stopped",
            RommScanOutcome::Failed => "failed",
            RommScanOutcome::TimedOut => "timed_out",
            RommScanOutcome::Unavailable => "unavailable",
            RommScanOutcome::Error => "error",
            RommScanOutcome::Unmapped => "unmapped",
        }
    }

    pub fn parse(s: &str) -> Option<RommScanOutcome> {
        RommScanOutcome::ALL.into_iter().find(|o| o.as_str() == s)
    }

    /// Whether the outcome is a warning on its Job.
    pub fn is_warning(self) -> bool {
        !matches!(self, RommScanOutcome::Requested | RommScanOutcome::Finished)
    }
}

/// The RomM scan a Build Job requested of the Library Folder it changed, recorded on the Job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommScan {
    /// The RomM platform the folder maps to: the `fs_slug` scanned, or why none was.
    pub platform: RommPlatform,
    pub outcome: RommScanOutcome,
    /// RomM's id for its scan task, once RomM accepted the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// What went wrong, for an outcome that is a warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub requested_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
    /// How RomM's "Additions" collection was kept in step once the scan finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<RommAdditions>,
}

/// How keeping RomM's "Additions" collection in step went. Anything but `synced` is a warning on the Job, never its
/// failure (ADR 0020).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RommAdditionsOutcome {
    /// The collection holds exactly the Additions RomM has indexed.
    Synced,
    /// Stage Select stopped before it finished: the Service stopped, or RomM was turned off or its address or root
    /// changed meanwhile.
    Stopped,
    /// A request was refused or went unanswered: RomM unreachable, the token refused or lacking a scope, or an
    /// answer that doesn't read.
    Error,
}

impl RommAdditionsOutcome {
    pub const ALL: [RommAdditionsOutcome; 3] = [
        RommAdditionsOutcome::Synced,
        RommAdditionsOutcome::Stopped,
        RommAdditionsOutcome::Error,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RommAdditionsOutcome::Synced => "synced",
            RommAdditionsOutcome::Stopped => "stopped",
            RommAdditionsOutcome::Error => "error",
        }
    }
}

/// RomM's "Additions" collection kept in step with every Addition built into a Library Folder RomM maps, after the
/// scan a Build Job requested finished (#142).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommAdditions {
    pub outcome: RommAdditionsOutcome,
    /// RomM's id for the collection, once it was found or created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_id: Option<i64>,
    /// How many of RomM's ROMs the collection holds now.
    pub members: u64,
    pub added: u64,
    pub removed: u64,
    /// The name of each built Addition RomM hasn't indexed, so isn't in the collection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<String>,
    /// What went wrong, for an outcome that is a warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub synced_at: String,
}

impl RommAdditions {
    /// Whether it is a warning on its Job: it didn't finish, or an Addition is unresolved.
    pub fn is_warning(&self) -> bool {
        self.outcome != RommAdditionsOutcome::Synced || !self.unresolved.is_empty()
    }
}

/// One Library Folder and the RomM platform it maps to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommFolderPlatform {
    pub folder_id: i64,
    pub root: String,
    /// Relative to its Root, with `/` separators.
    pub path: String,
    pub platform: RommPlatform,
}

/// A way RomM's view of a file a Build made disagrees with the Build. Stage Select only shows these; it never writes
/// identity into RomM (ADR 0020).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RommFlag {
    /// RomM matched the file's game to no metadata provider.
    Unidentified,
    /// RomM's matched name isn't the DAT Game's, once tags, case and punctuation are set aside.
    TitleMismatch,
    /// RomM's SHA-1 for the file isn't what was built, or RomM lacks the file.
    HashMismatch,
}

impl RommFlag {
    pub const ALL: [RommFlag; 3] = [
        RommFlag::Unidentified,
        RommFlag::TitleMismatch,
        RommFlag::HashMismatch,
    ];
}

/// One file a Build made in a Library Folder that RomM sees otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommReconcileRow {
    /// The file's path in the folder, with `/` separators.
    pub target: String,
    /// The DAT Game it was built for, or the release, for an `.m3u`.
    pub game: String,
    /// Its flags, in [`RommFlag::ALL`] order.
    pub flags: Vec<RommFlag>,
    /// RomM's id for the ROM holding the file, when RomM has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rom_id: Option<i64>,
    /// The name RomM matched the ROM to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub romm_name: Option<String>,
    /// What the hash mismatch is, for a row that has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// How many files a reconciliation compared, and how many of them carry each flag. A file may carry more than one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommReconcileCounts {
    pub files: u64,
    pub clean: u64,
    pub unidentified: u64,
    pub title_mismatch: u64,
    pub hash_mismatch: u64,
}

/// The latest comparison of RomM's view of a Library Folder with the files its Builds made. Only the latest is kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommReconciliation {
    pub folder_id: i64,
    /// The Job whose RomM scan it followed, if a scan led to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<i64>,
    /// The RomM platform compared.
    pub fs_slug: String,
    pub reconciled_at: String,
    /// Why it couldn't be done, if it couldn't: then it has no counts or rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub counts: RommReconcileCounts,
    /// Each file with a flag, by target.
    pub rows: Vec<RommReconcileRow>,
}

/// What `POST /libraries/{id}/romm/rescans` asks RomM to scan again: one ROM of the folder's latest reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RommRescanRequest {
    pub rom_id: i64,
}

/// The rescan RomM accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommRescan {
    pub rom_id: i64,
    /// RomM's scan type: `unmatched` for an unidentified ROM, `update` otherwise.
    pub scan_type: String,
    pub task_id: String,
}

/// Sent by `/events` each time a comparison of RomM's view of a Library Folder is kept, so a client showing it can
/// fetch it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RommReconciled {
    pub folder_id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_5_4_releases_are_tested() {
        for tested in ["5.4", "5.4.0", "5.4.1", "v5.4.12", " 5.4.1\n"] {
            assert!(version_tested(tested), "{tested}");
        }
        for untested in [
            "5.5.0-alpha.1",
            "5.40.0",
            "5.3.9",
            "5.4.1-rc.1",
            "5.4.",
            "development",
            "",
        ] {
            assert!(!version_tested(untested), "{untested}");
        }
    }
}
