//! The permission policy's form and the permissions view, as pure logic: what you typed read as a policy, and what a
//! check found said in words.

use stage_select_api::permissions::{
    Mode, PermissionCause, PermissionDifference, PermissionPolicy, PermissionsRestored,
};

/// A policy as typed: a file mode, a folder mode and a group, each as text. All empty is no policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyDraft {
    pub file_mode: String,
    pub folder_mode: String,
    pub group: String,
}

impl PolicyDraft {
    pub fn of(policy: Option<PermissionPolicy>) -> PolicyDraft {
        match policy {
            None => PolicyDraft::default(),
            Some(p) => PolicyDraft {
                file_mode: p.file_mode.to_string(),
                folder_mode: p.folder_mode.to_string(),
                group: p.group.map(|g| g.to_string()).unwrap_or_default(),
            },
        }
    }

    /// The policy typed, none when every field is empty, or what is wrong with it; `what` names it in a sentence.
    pub fn read(&self, what: &str) -> Result<Option<PermissionPolicy>, String> {
        let (file, folder, group) = (
            self.file_mode.trim(),
            self.folder_mode.trim(),
            self.group.trim(),
        );
        if file.is_empty() && folder.is_empty() && group.is_empty() {
            return Ok(None);
        }
        let mode = |typed: &str, which: &str, example: &str| {
            Mode::parse(typed).ok_or_else(|| {
                format!("Give {what} a {which} mode of octal digits, such as {example}.")
            })
        };
        let file_mode = mode(file, "file", "0660")?;
        let folder_mode = mode(folder, "folder", "2770")?;
        let group = match group {
            "" => None,
            g => Some(g.parse::<u32>().map_err(|_| {
                format!("Give {what} a group by its number, as id -g shows it, or leave it empty.")
            })?),
        };
        Ok(Some(PermissionPolicy {
            file_mode,
            folder_mode,
            group,
        }))
    }
}

/// A policy in words: "0660 files, 2770 folders, group 1000".
pub fn policy_line(policy: &PermissionPolicy) -> String {
    let mut line = format!("{} files, {} folders", policy.file_mode, policy.folder_mode);
    if let Some(group) = policy.group {
        line.push_str(&format!(", group {group}"));
    }
    line
}

/// "3 files and 1 folder".
fn things(files: usize, folders: usize) -> String {
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

/// Who can restore a difference, terse, for its cell.
pub fn cause_label(cause: PermissionCause) -> &'static str {
    match cause {
        PermissionCause::Restorable => "Stage Select",
        PermissionCause::OtherOwner => "Another owner",
        PermissionCause::NotInGroup => "Not its group",
        PermissionCause::ReadOnly => "Read-only",
    }
}

/// Why a difference is its cause's, for the detail pane.
pub fn cause_reason(cause: PermissionCause) -> &'static str {
    match cause {
        PermissionCause::Restorable => {
            "Stage Select owns it, so the plan restores it once you approve it."
        }
        PermissionCause::OtherOwner => {
            "Another user owns it, so Stage Select can't change its mode. Run the command above on the host."
        }
        PermissionCause::NotInGroup => {
            "Stage Select isn't in your policy's group, so it can't give it that group. Run the command above on the \
             host."
        }
        PermissionCause::ReadOnly => {
            "Stage Select may not change this source set, so it only reports it. Run the command above on the host."
        }
    }
}

/// A file's mode and group, as a cell shows it: "0644, group 20".
pub fn shown(mode: Mode, group: Option<u32>) -> String {
    match group {
        Some(g) => format!("{mode}, group {g}"),
        None => mode.to_string(),
    }
}

/// What a difference is now and what the policy gives it, each as [`shown`] says, the group only when the policy
/// sets one.
pub fn change(d: &PermissionDifference) -> (String, String) {
    let group = d.expected_group.map(|_| d.gid);
    (
        shown(d.mode, group),
        shown(d.expected_mode, d.expected_group),
    )
}

/// The plan's line: what it restores, in numbers.
pub fn plan_line(differences: &[PermissionDifference]) -> Option<String> {
    let restorable: Vec<_> = differences
        .iter()
        .filter(|d| d.cause == PermissionCause::Restorable)
        .collect();
    if restorable.is_empty() {
        return None;
    }
    let folders = restorable.iter().filter(|d| d.folder).count();
    Some(format!(
        "Restores the permissions of {}.",
        things(restorable.len() - folders, folders)
    ))
}

/// What approving the plan says on its button.
pub fn approve_label(differences: &[PermissionDifference]) -> String {
    let n = differences
        .iter()
        .filter(|d| d.cause == PermissionCause::Restorable)
        .count();
    format!("Approve and restore {n}")
}

/// What a restore did, in a sentence.
pub fn restored_line(restored: &PermissionsRestored) -> String {
    let done = restored.restored;
    match restored.failed.len() {
        0 => format!("Restored the permissions of {done}."),
        failed => format!(
            "Restored the permissions of {done}; {failed} couldn't be restored: {}.",
            restored
                .failed
                .iter()
                .map(|f| format!("{} ({})", f.path, f.reason))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

/// The line over what a check found: how many differ of how many compared.
pub fn found_line(differing: usize, compared: u64) -> String {
    if differing == 0 {
        format!("All {compared} files and folders checked match your permission policy.")
    } else {
        format!(
            "{differing} of the {compared} files and folders checked differ from your permission policy."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stage_select_api::permissions::PermissionFailure;

    #[test]
    fn a_draft_reads_as_a_policy_none_or_what_is_wrong() {
        let draft = PolicyDraft {
            file_mode: "660".into(),
            folder_mode: " 2770 ".into(),
            group: "1000".into(),
        };
        let policy = draft.read("every root").unwrap().unwrap();
        assert_eq!(policy.file_mode, Mode(0o660));
        assert_eq!(policy.group, Some(1000));
        assert_eq!(PolicyDraft::of(Some(policy)).file_mode, "0660");
        assert_eq!(PolicyDraft::default().read("every root"), Ok(None));
        assert_eq!(
            PolicyDraft {
                file_mode: "rw".into(),
                ..draft.clone()
            }
            .read("every root"),
            Err("Give every root a file mode of octal digits, such as 0660.".into())
        );
        assert!(
            PolicyDraft {
                group: "gaming".into(),
                ..draft
            }
            .read("play")
            .unwrap_err()
            .contains("by its number")
        );
    }

    #[test]
    fn what_a_check_found_is_said_in_numbers() {
        let d = |folder, cause| PermissionDifference {
            path: "a".into(),
            folder,
            mode: Mode(0o644),
            uid: 1,
            gid: 20,
            expected_mode: Mode(0o660),
            expected_group: Some(100),
            cause,
        };
        let found = [
            d(false, PermissionCause::Restorable),
            d(true, PermissionCause::Restorable),
            d(false, PermissionCause::OtherOwner),
        ];
        assert_eq!(
            plan_line(&found).as_deref(),
            Some("Restores the permissions of 1 file and 1 folder.")
        );
        assert_eq!(approve_label(&found), "Approve and restore 2");
        assert_eq!(
            change(&found[0]),
            ("0644, group 20".into(), "0660, group 100".into())
        );
        assert_eq!(plan_line(&found[2..]), None);
        assert_eq!(
            restored_line(&PermissionsRestored {
                restored: 1,
                failed: vec![PermissionFailure {
                    path: "b".into(),
                    reason: "it can't be found".into()
                }],
            }),
            "Restored the permissions of 1; 1 couldn't be restored: b (it can't be found)."
        );
        assert_eq!(
            found_line(0, 4),
            "All 4 files and folders checked match your permission policy."
        );
    }
}
