//! The Permission Policy (ADR 0031): the file mode, folder mode and optional group the Operator intends for a writable
//! Root's files, global with per-Root overrides, kept with the settings (ADR 0029); and what a check finds of the
//! files that differ from it, apart from Stale and Drift.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Permission bits, as `chmod` takes them: written as octal digits, `"2770"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Mode(pub u32);

impl Mode {
    /// The octal digits of a mode as typed, `"660"` or `"0660"`, or none if they aren't a mode.
    pub fn parse(s: &str) -> Option<Mode> {
        let s = s.trim();
        if s.is_empty() || s.len() > 5 || !s.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            return None;
        }
        u32::from_str_radix(s, 8)
            .ok()
            .filter(|m| *m <= 0o7777)
            .map(Mode)
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04o}", self.0)
    }
}

impl Serialize for Mode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Mode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Mode::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!("{s:?} is not a mode of octal digits, such as 0660"))
        })
    }
}

/// What the Operator intends for the files and folders of a Root: their mode, and their group when one is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionPolicy {
    /// Setuid, setgid and sticky aside, as `0660`.
    pub file_mode: Mode,
    /// Setgid included, as `2770`.
    pub folder_mode: Mode,
    /// The group, by its number; none keeps whatever group each file gets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<u32>,
}

impl PermissionPolicy {
    /// The mode it gives a folder, or a file when not `folder`.
    pub fn mode(&self, folder: bool) -> u32 {
        if folder {
            self.folder_mode.0
        } else {
            self.file_mode.0
        }
    }
}

/// The Permission Policy, as the settings hold it: one for every Root, and each Root's own in its place. Without
/// either, a Root's files keep the Host's defaults and nothing is reported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub global: Option<PermissionPolicy>,
    /// Each Root's own, by its name, in place of the global one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roots: BTreeMap<String, PermissionPolicy>,
}

impl PermissionSettings {
    /// The policy in force for the Root `root`: its own, or the global one.
    pub fn of(&self, root: &str) -> Option<PermissionPolicy> {
        self.roots.get(root).copied().or(self.global)
    }
}

/// Why a file that differs from the policy is as it is, and so who can restore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionCause {
    /// Stage Select owns it and may give it the policy: an approved plan restores it.
    Restorable,
    /// Another user owns it, so Stage Select can't change its mode.
    OtherOwner,
    /// Stage Select isn't in the policy's group, so it can't give it that group.
    NotInGroup,
    /// It is in a source set Stage Select may not change.
    ReadOnly,
}

impl PermissionCause {
    /// Every cause, those Stage Select can't restore itself first.
    pub const ALL: [PermissionCause; 4] = [
        PermissionCause::OtherOwner,
        PermissionCause::NotInGroup,
        PermissionCause::ReadOnly,
        PermissionCause::Restorable,
    ];
}

/// How many files and folders, in words: "3 files and 1 folder".
pub fn files_and_folders(files: usize, folders: usize) -> String {
    let n = |count: usize, one: &str, many: &str| {
        format!("{count} {}", if count == 1 { one } else { many })
    };
    match (files, folders) {
        (f, 0) => n(f, "file", "files"),
        (0, d) => n(d, "folder", "folders"),
        (f, d) => format!(
            "{} and {}",
            n(f, "file", "files"),
            n(d, "folder", "folders")
        ),
    }
}

/// A file or folder whose mode or group differs from the policy, as the check saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionDifference {
    /// Its path in the folder checked; empty for the folder itself.
    pub path: String,
    pub folder: bool,
    pub mode: Mode,
    pub uid: u32,
    pub gid: u32,
    /// What the policy gives it.
    pub expected_mode: Mode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_group: Option<u32>,
    pub cause: PermissionCause,
}

/// The differences of one cause Stage Select can't restore itself, with what to run on the Host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionFix {
    pub cause: PermissionCause,
    pub count: u64,
    /// What is wrong, what is expected and who runs the command, in sentences.
    pub explanation: String,
    /// The one command that fixes them all, to copy.
    pub command: String,
}

/// What the latest check of a folder found of its permissions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionsReport {
    /// The folder checked, as Stage Select sees it.
    pub path: String,
    pub checked_at: String,
    /// The policy it was checked against.
    pub policy: PermissionPolicy,
    /// Whether the policy is still the one in force: when not, the folder is to be checked again.
    pub current: bool,
    /// How many files and folders were compared.
    pub compared: u64,
    pub differences: Vec<PermissionDifference>,
    /// For each cause Stage Select can't restore itself, what to run on the Host.
    pub fixes: Vec<PermissionFix>,
    /// What the plan to restore the [`PermissionCause::Restorable`] differences binds: approve it with this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_digest: Option<String>,
}

/// A folder's permissions as `GET …/permissions` gives them: the policy in force, and what its latest check since
/// Stage Select started found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionsView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PermissionPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<PermissionsReport>,
}

impl PermissionsView {
    /// How many files and folders differ from the policy, by the latest check against the policy in force.
    pub fn differing(&self) -> u64 {
        self.report
            .as_ref()
            .filter(|r| r.current)
            .map(|r| r.differences.len() as u64)
            .unwrap_or(0)
    }
}

/// Approving the plan that restores a folder's permissions, as `POST …/permissions/restore` takes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionsApproval {
    pub digest: String,
}

/// One file or folder the plan could not restore, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionFailure {
    pub path: String,
    pub reason: String,
}

/// What restoring a folder's permissions did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionsRestored {
    pub restored: u64,
    pub failed: Vec<PermissionFailure>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mode_is_written_and_read_as_octal_digits() {
        assert_eq!(Mode::parse("2770"), Some(Mode(0o2770)));
        assert_eq!(Mode::parse("660"), Some(Mode(0o660)));
        assert_eq!(Mode(0o660).to_string(), "0660");
        for bad in ["", "8", "17777", "rw-", "-1", "77777"] {
            assert_eq!(Mode::parse(bad), None, "{bad}");
        }
        let policy: PermissionPolicy =
            serde_json::from_str(r#"{"file_mode": "0660", "folder_mode": "2770", "group": 100}"#)
                .unwrap();
        assert_eq!(policy.mode(true), 0o2770);
        assert_eq!(
            serde_json::to_string(&policy).unwrap(),
            r#"{"file_mode":"0660","folder_mode":"2770","group":100}"#
        );
    }

    #[test]
    fn a_roots_own_policy_takes_the_place_of_the_global_one() {
        let global = PermissionPolicy {
            file_mode: Mode(0o644),
            folder_mode: Mode(0o755),
            group: None,
        };
        let own = PermissionPolicy {
            group: Some(7),
            ..global
        };
        let settings = PermissionSettings {
            global: Some(global),
            roots: [("play".to_owned(), own)].into(),
        };
        assert_eq!(settings.of("play"), Some(own));
        assert_eq!(settings.of("sets"), Some(global));
        assert_eq!(PermissionSettings::default().of("play"), None);
    }
}
