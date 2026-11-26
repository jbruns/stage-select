//! The Permission Policy (ADR 0031): given to what Stage Select writes, compared with the stats a stat walk or a
//! Library Folder check already takes, reported apart from Stale and Drift, and restored through an approved plan
//! where Stage Select's own privileges reach. It changes modes only of entries it owns and groups only to groups it
//! is in; for the rest it says what to run on the Host.
//!
//! What a check found is held in memory, by folder, until the next check of it or until the service stops: a check
//! is cheap to run again, and the plan to restore is the latest check's, bound by its digest.

use std::collections::HashMap;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat};
use rustix::process::Gid;
use stage_select_api::permissions::{
    Mode as PolicyMode, PermissionCause, PermissionDifference, PermissionFailure, PermissionFix,
    PermissionPolicy, PermissionSettings, PermissionsReport, PermissionsRestored, PermissionsView,
    files_and_folders,
};

use crate::folder_dir::{FolderDir, Identity, owner_of, owner_of_id, quoted};
use crate::roots::{Root, Roots};
use crate::{Error, Result, Store, now};

/// Stage Select's user, and every group it is in.
struct Me {
    uid: u32,
    groups: Vec<u32>,
}

impl Me {
    fn now() -> Me {
        let mut groups = vec![rustix::process::getegid().as_raw()];
        if let Ok(more) = rustix::process::getgroups() {
            groups.extend(more.iter().map(|g| g.as_raw()));
        }
        Me {
            uid: rustix::process::geteuid().as_raw(),
            groups,
        }
    }

    fn root(&self) -> bool {
        self.uid == 0
    }

    fn in_group(&self, gid: u32) -> bool {
        self.root() || self.groups.contains(&gid)
    }
}

/// Refuses a Permission Policy that would keep Stage Select from its own files, or give them a group it can't, or one
/// for a Root that isn't registered.
pub(crate) fn check_settings(roots: &Roots, settings: &PermissionSettings) -> Result<()> {
    let refuse = |why: String| Err(Error::Refused(why));
    let me = Me::now();
    let mut policies: Vec<(String, &PermissionPolicy)> = Vec::new();
    if let Some(global) = &settings.global {
        policies.push(("the policy for every root".into(), global));
    }
    for (name, policy) in &settings.roots {
        if roots.root(name).is_none() {
            return refuse(format!("There is no root named {name}."));
        }
        policies.push((format!("the policy for {name}"), policy));
    }
    for (what, policy) in policies {
        if policy.file_mode.0 > 0o777 {
            return refuse(format!(
                "Give files a mode of at most 0777 in {what}: setuid, setgid and sticky bits are for folders."
            ));
        }
        if policy.file_mode.0 & 0o600 != 0o600 {
            return refuse(format!(
                "Let the owner read and write files in {what} (such as 0660): Stage Select writes them as their \
                 owner."
            ));
        }
        if policy.folder_mode.0 & 0o700 != 0o700 {
            return refuse(format!(
                "Let the owner read, write and open folders in {what} (such as 2770): Stage Select writes in them \
                 as their owner."
            ));
        }
        if let Some(group) = policy.group
            && !me.in_group(group)
        {
            return refuse(format!(
                "Stage Select isn't in group {group}, so it can't give files that group ({what}). Add its user to \
                 the group on the host and restart it, or choose a group it is in."
            ));
        }
    }
    Ok(())
}

/// The permission bits of `st`.
// `st_mode` is a `u16` on macOS and a `u32` on Linux.
#[allow(clippy::unnecessary_cast)]
fn mode_of(st: &Stat) -> u32 {
    st.st_mode as u32 & 0o7777
}

/// Whether `st` is as `policy` says.
fn as_declared(st: &Stat, policy: &PermissionPolicy) -> bool {
    let folder = FileType::from_raw_mode(st.st_mode) == FileType::Directory;
    mode_of(st) == policy.mode(folder) && policy.group.is_none_or(|g| st.st_gid == g)
}

/// Gives the entry `name` in the folder `dir`, or the entry open as `dir` itself when `name` is none, the policy: its
/// group first, where Stage Select is in that group and owns the entry, then its mode, where it owns it. A named entry
/// is opened without following a symlink and changed only through that descriptor, so an entry swapped for a symlink
/// meanwhile is never followed; a symlink, anything but a regular file or a folder, and an entry Stage Select can't
/// open are left as they are. Never fails what it is part of: an entry that comes out other than declared, as a
/// filesystem or a Host's access control lists may make it, is what the next check reports. Returns the `lstat`
/// after, if it could be taken.
pub(crate) fn give(dir: BorrowedFd, name: Option<&str>, policy: &PermissionPolicy) -> Option<Stat> {
    let Some(name) = name else {
        return give_fd(dir, policy);
    };
    let flags =
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    match rustix::fs::openat(dir, name, flags, Mode::empty()) {
        Ok(fd) => give_fd(fd.as_fd(), policy),
        // One whose mode keeps even its owner from reading it, which its owner may still change.
        Err(rustix::io::Errno::ACCESS) => give_unread(dir, name, policy),
        Err(_) => rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok(),
    }
}

/// Gives the entry `name` in `dir`, which can't be opened to read, the policy as [`give`] says: through a descriptor
/// that only names it, opened without following a symlink.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn give_unread(dir: BorrowedFd, name: &str, policy: &PermissionPolicy) -> Option<Stat> {
    let flags = OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = rustix::fs::openat(dir, name, flags, Mode::empty()).ok()?;
    // A descriptor opened only to name an entry can't be changed itself; its link in /proc can, and leads to the
    // entry it names, never further, a symlink having been refused above by its type.
    let named = format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&fd));
    give_with(
        &fd,
        policy,
        |group| rustix::fs::chownat(&fd, "", None, Some(group), AtFlags::EMPTY_PATH),
        |mode| rustix::fs::chmodat(rustix::fs::CWD, named.as_str(), mode, AtFlags::empty()),
    )
}

/// Gives the entry `name` in `dir`, which can't be opened to read, the policy as [`give`] says: by its name, never
/// following a symlink, so one swapped in meanwhile is changed itself and its target never.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn give_unread(dir: BorrowedFd, name: &str, policy: &PermissionPolicy) -> Option<Stat> {
    let nofollow = AtFlags::SYMLINK_NOFOLLOW;
    let stat = || rustix::fs::statat(dir, name, nofollow);
    let st = stat().ok()?;
    if !matches!(
        FileType::from_raw_mode(st.st_mode),
        FileType::RegularFile | FileType::Directory
    ) {
        return Some(st);
    }
    let me = Me::now();
    if !(me.root() || st.st_uid == me.uid) {
        return Some(st);
    }
    if let Some(group) = policy.group
        && st.st_gid != group
        && me.in_group(group)
    {
        let _ = rustix::fs::chownat(dir, name, None, Some(Gid::from_raw(group)), nofollow);
    }
    let st = stat().ok()?;
    let want = policy.mode(FileType::from_raw_mode(st.st_mode) == FileType::Directory);
    if mode_of(&st) != want {
        let _ = rustix::fs::chmodat(dir, name, Mode::from_raw_mode(want as _), nofollow);
    }
    stat().ok()
}

/// Gives the regular file or folder open as `fd` the policy, as [`give`] says.
fn give_fd(fd: BorrowedFd, policy: &PermissionPolicy) -> Option<Stat> {
    give_with(
        fd,
        policy,
        |group| rustix::fs::fchown(fd, None, Some(group)),
        |mode| rustix::fs::fchmod(fd, mode),
    )
}

/// Gives the regular file or folder `fd` names the policy, as [`give`] says, changing its group by `chgrp` and its
/// mode by `chmod`.
fn give_with(
    fd: impl AsFd,
    policy: &PermissionPolicy,
    chgrp: impl Fn(Gid) -> rustix::io::Result<()>,
    chmod: impl Fn(Mode) -> rustix::io::Result<()>,
) -> Option<Stat> {
    let fd = fd.as_fd();
    let st = rustix::fs::fstat(fd).ok()?;
    let folder = match FileType::from_raw_mode(st.st_mode) {
        FileType::Directory => true,
        FileType::RegularFile => false,
        _ => return Some(st),
    };
    let me = Me::now();
    if !(me.root() || st.st_uid == me.uid) {
        return Some(st);
    }
    if let Some(group) = policy.group
        && st.st_gid != group
        && me.in_group(group)
    {
        let _ = chgrp(Gid::from_raw(group));
    }
    // A change of group may clear setgid, so the mode is set after it, from what it is then.
    let st = rustix::fs::fstat(fd).ok()?;
    let want = policy.mode(folder);
    if mode_of(&st) != want {
        let _ = chmod(Mode::from_raw_mode(want as _));
    }
    rustix::fs::fstat(fd).ok()
}

/// A file or folder as a check saw it: what its permissions are compared from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Seen {
    /// Its path in the folder checked; empty for the folder itself.
    pub rel: String,
    pub folder: bool,
    pub id: Identity,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

impl Seen {
    /// From an `lstat`: a regular file or a folder, else none.
    pub fn of(rel: impl Into<String>, st: &Stat) -> Option<Seen> {
        let folder = match FileType::from_raw_mode(st.st_mode) {
            FileType::Directory => true,
            FileType::RegularFile => false,
            _ => return None,
        };
        let (uid, gid) = owner_of(st);
        Some(Seen {
            rel: rel.into(),
            folder,
            id: Identity::of(st),
            mode: mode_of(st),
            uid,
            gid,
        })
    }

    /// From what a stat walk saw.
    pub fn of_perm(rel: impl Into<String>, folder: bool, perm: &stage_select_engine::Perm) -> Seen {
        let id = Identity {
            dev: perm.dev,
            ino: perm.ino,
        };
        let (uid, gid) = owner_of_id(id, (perm.uid, perm.gid));
        Seen {
            rel: rel.into(),
            folder,
            id,
            mode: perm.mode,
            uid,
            gid,
        }
    }
}

/// The entries of `seen` that differ from `policy`, each with why: in a folder Stage Select may write to when
/// `writable`, else in a read-only one.
fn compare(
    policy: &PermissionPolicy,
    writable: bool,
    me: &Me,
    seen: Vec<Seen>,
) -> Vec<(Seen, PermissionDifference)> {
    let mut found: Vec<_> = seen
        .into_iter()
        .filter_map(|s| {
            let mode_differs = s.mode != policy.mode(s.folder);
            let group_differs = policy.group.is_some_and(|g| s.gid != g);
            if !mode_differs && !group_differs {
                return None;
            }
            let cause = if !writable {
                PermissionCause::ReadOnly
            } else if me.root() {
                PermissionCause::Restorable
            } else if s.uid != me.uid {
                PermissionCause::OtherOwner
            } else if policy
                .group
                .is_some_and(|g| group_differs && !me.in_group(g))
            {
                PermissionCause::NotInGroup
            } else {
                PermissionCause::Restorable
            };
            let difference = PermissionDifference {
                path: s.rel.clone(),
                folder: s.folder,
                mode: PolicyMode(s.mode),
                uid: s.uid,
                gid: s.gid,
                expected_mode: PolicyMode(policy.mode(s.folder)),
                expected_group: policy.group,
                cause,
            };
            Some((s, difference))
        })
        .collect();
    found.sort_by(|a, b| a.0.rel.cmp(&b.0.rel));
    found.dedup_by(|a, b| a.0.rel == b.0.rel);
    found
}

/// What the policy is, in words.
fn declared(policy: &PermissionPolicy) -> String {
    let mut said = format!(
        "{} for files and {} for folders",
        policy.file_mode, policy.folder_mode
    );
    if let Some(group) = policy.group {
        said.push_str(&format!(", in group {group}"));
    }
    said
}

/// Where a command's path comes from, as every command Stage Select gives says.
const AS_SEEN: &str = "the path is as Stage Select sees it: if Stage Select runs in a container, use the host \
                       folder mounted there";

/// For each cause Stage Select can't restore itself, what is wrong, what is expected and the one command to run on
/// the Host for all of the folder at `path`.
fn fixes(
    path: &Path,
    policy: &PermissionPolicy,
    me: &Me,
    found: &[PermissionDifference],
) -> Vec<PermissionFix> {
    let at = quoted(path);
    // Every file and folder in it, a link never followed, and never a `.stage-select` folder or what is in it: those
    // keep their own rule, which a command for the rest would open to others.
    let each = |then: &str| format!("find {at} -name .stage-select -prune -o {then}");
    let all = |command: String| each(&format!(r"\( -type d -o -type f \) -exec {command} {{}} +"));
    PermissionCause::ALL
        .into_iter()
        .filter(|c| *c != PermissionCause::Restorable)
        .filter_map(|cause| {
            let of: Vec<_> = found.iter().filter(|d| d.cause == cause).collect();
            if of.is_empty() {
                return None;
            }
            let folders = of.iter().filter(|d| d.folder).count();
            let count = files_and_folders(of.len() - folders, folders);
            // "1 file here belongs", "2 files here belong".
            let one = of.len() == 1;
            let (belong, are_not, differ, them) = if one {
                ("belongs", "isn't", "differs", "it")
            } else {
                ("belong", "aren't", "differ", "them")
            };
            let expected = declared(policy);
            let (explanation, command) = match cause {
                PermissionCause::OtherOwner => (
                    format!(
                        "{count} here {belong} to another user, so Stage Select can't give {them} the permissions you \
                         chose: {expected}. To let it, run this on the host as root, then check again ({AS_SEEN}). \
                         Stage Select can then restore {them} through a plan you approve."
                    ),
                    all(format!("chown {}", me.uid)),
                ),
                PermissionCause::NotInGroup => {
                    let group = policy.group.unwrap_or_default();
                    (
                        format!(
                            "{count} here {are_not} in group {group}, and Stage Select isn't in that group, so it can't \
                             give it to {them}: {expected}. Run this on the host as root, then check again \
                             ({AS_SEEN})."
                        ),
                        all(format!("chgrp {group}")),
                    )
                }
                PermissionCause::ReadOnly => {
                    let modes = each(&format!(
                        "-type d -exec chmod {} {{}} + -o -type f -exec chmod {} {{}} +",
                        policy.folder_mode, policy.file_mode
                    ));
                    (
                        format!(
                            "{count} here {differ} from the permissions you chose: {expected}. Stage Select may not \
                             change this source set, so it only reports {them}. To fix {them}, run this on the host as \
                             their owner or as root, then check again ({AS_SEEN})."
                        ),
                        match policy.group {
                            Some(group) => format!("{} && {modes}", all(format!("chgrp {group}"))),
                            None => modes,
                        },
                    )
                }
                PermissionCause::Restorable => unreachable!("filtered out"),
            };
            Some(PermissionFix {
                cause,
                count: of.len() as u64,
                explanation,
                command,
            })
        })
        .collect()
}

/// A folder whose permissions a check compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Scope {
    SourceSet(i64),
    Library(i64),
}

/// What the latest check of a folder found, until the next.
#[derive(Debug, Clone)]
pub(crate) struct Held {
    root: String,
    rel: String,
    path: PathBuf,
    writable: bool,
    policy: PermissionPolicy,
    checked_at: String,
    compared: u64,
    found: Vec<(Seen, PermissionDifference)>,
}

/// Every folder's latest check, by folder.
pub(crate) type Checked = std::sync::Mutex<HashMap<Scope, Held>>;

/// What the plan to restore `found` binds: each entry Stage Select can restore, by path and identity, as it was seen.
fn digest(found: &[(Seen, PermissionDifference)]) -> Option<String> {
    let items: Vec<_> = found
        .iter()
        .filter(|(_, d)| d.cause == PermissionCause::Restorable)
        .map(|(s, d)| (d, s.id.dev, s.id.ino))
        .collect();
    if items.is_empty() {
        return None;
    }
    let bytes = serde_json::to_vec(&items).expect("differences serialize");
    Some(stage_select_engine::sha256_hex(&bytes))
}

impl Store {
    fn checked(&self) -> std::sync::MutexGuard<'_, HashMap<Scope, Held>> {
        self.permissions.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records what a check of the folder `rel` in `root` saw of its entries, `seen`, against the Permission Policy in
    /// force for `root`, replacing the last. With no policy, nothing is compared and nothing is reported.
    pub(crate) fn record_permissions(
        &self,
        scope: Scope,
        root: &Root,
        rel: &str,
        writable: bool,
        seen: Vec<Seen>,
    ) {
        let Some(policy) = root.permissions else {
            self.checked().remove(&scope);
            return;
        };
        let compared = seen.len() as u64;
        let found = compare(&policy, writable, &Me::now(), seen);
        let path = rel
            .split('/')
            .filter(|p| !p.is_empty())
            .fold(root.path.clone(), |path, part| path.join(part));
        self.checked().insert(
            scope,
            Held {
                root: root.name.clone(),
                rel: rel.to_owned(),
                path,
                writable,
                policy,
                checked_at: now(),
                compared,
                found,
            },
        );
    }

    /// The folder's permissions: the policy in force now for `root`, and what its latest check found.
    fn permissions_view(&self, scope: Scope, root: Option<&Root>) -> PermissionsView {
        let policy = root.and_then(|r| r.permissions);
        let report = self.checked().get(&scope).map(|held| {
            let current = policy == Some(held.policy) && root.is_some_and(|r| r.name == held.root);
            let differences: Vec<_> = held.found.iter().map(|(_, d)| d.clone()).collect();
            PermissionsReport {
                path: held.path.display().to_string(),
                checked_at: held.checked_at.clone(),
                policy: held.policy,
                current,
                compared: held.compared,
                fixes: fixes(&held.path, &held.policy, &Me::now(), &differences),
                differences,
                plan_digest: if current && held.writable {
                    digest(&held.found)
                } else {
                    None
                },
            }
        });
        PermissionsView { policy, report }
    }

    /// Restores, as `actor`, the permissions of the folder's entries that its latest check found Stage Select can
    /// restore, once `digest` shows the plan approved is that check's: each one while it is still the entry the
    /// check saw. Refused when the policy or the folder's writability changed since.
    fn restore_permissions(
        &self,
        scope: Scope,
        root: &Root,
        writable: bool,
        digest_given: &str,
        _actor: &str,
    ) -> Result<PermissionsRestored> {
        let mut checked = self.checked();
        let stale = || {
            Error::Conflict(
                "The permissions changed since you reviewed them, so nothing was changed. Check again, then review \
                 them again."
                    .into(),
            )
        };
        let held = checked.get_mut(&scope).ok_or_else(stale)?;
        if root.permissions != Some(held.policy)
            || root.name != held.root
            || !writable
            || !held.writable
            || digest(&held.found).as_deref() != Some(digest_given)
        {
            return Err(stale());
        }
        let dir = FolderDir::open(&root.path, &held.rel)
            .map_err(|e| Error::Refused(format!("{}: {e}", held.path.display())))?;
        let mut outcome = PermissionsRestored::default();
        let policy = held.policy;
        held.found.retain(|(seen, difference)| {
            if difference.cause != PermissionCause::Restorable {
                return true;
            }
            let shown = if seen.rel.is_empty() {
                held.path.display().to_string()
            } else {
                seen.rel.clone()
            };
            match dir.restore(&seen.rel, seen.id, &policy) {
                Ok(st) if as_declared(&st, &policy) => {
                    outcome.restored += 1;
                    false
                }
                Ok(st) => {
                    outcome.failed.push(PermissionFailure {
                        path: shown,
                        reason: format!(
                            "the filesystem kept it at {}{}",
                            PolicyMode(mode_of(&st)),
                            if policy.group.is_some_and(|g| st.st_gid != g) {
                                format!(" in group {}", st.st_gid)
                            } else {
                                String::new()
                            }
                        ),
                    });
                    true
                }
                Err(reason) => {
                    outcome.failed.push(PermissionFailure {
                        path: shown,
                        reason,
                    });
                    true
                }
            }
        });
        Ok(outcome)
    }

    /// The Source Set `name`'s permissions, by its latest stat walk since Stage Select started.
    pub fn source_set_permissions(&self, name: &str) -> Result<PermissionsView> {
        let set = self.read(|c| crate::sets::set_by_name(c, name))?;
        let roots = self.roots();
        Ok(self.permissions_view(Scope::SourceSet(set.id), roots.root(&set.record.root)))
    }

    /// Restores, as `actor`, what the latest stat walk of the Source Set `name` found Stage Select can restore, by
    /// the plan whose digest is `digest`. Refused for a Source Set Stage Select may not change.
    pub fn restore_source_set_permissions(
        &self,
        name: &str,
        digest: &str,
        actor: &str,
    ) -> Result<PermissionsRestored> {
        let set = self.read(|c| crate::sets::set_by_name(c, name))?;
        let roots = self.roots();
        let root = roots.root(&set.record.root).ok_or_else(|| {
            Error::Refused(format!("There is no root named {}.", set.record.root))
        })?;
        let writable = root.may_write(&set.record.path);
        self.restore_permissions(Scope::SourceSet(set.id), root, writable, digest, actor)
    }

    /// The Library Folder `id`'s permissions, by its latest check since Stage Select started.
    pub fn library_permissions(&self, id: i64) -> Result<PermissionsView> {
        let folder = self.read(|c| crate::folders::folder_by_id(c, id))?;
        let roots = self.roots();
        Ok(self.permissions_view(Scope::Library(id), roots.root(&folder.root)))
    }

    /// Restores, as `actor`, what the latest check of the Library Folder `id` found Stage Select can restore, by the
    /// plan whose digest is `digest`.
    pub fn restore_library_permissions(
        &self,
        id: i64,
        digest: &str,
        actor: &str,
    ) -> Result<PermissionsRestored> {
        let folder = self.read(|c| crate::folders::folder_by_id(c, id))?;
        let roots = self.roots();
        let root = roots
            .root(&folder.root)
            .ok_or_else(|| Error::Refused(format!("There is no root named {}.", folder.root)))?;
        self.restore_permissions(Scope::Library(id), root, root.writable, digest, actor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POLICY: PermissionPolicy = PermissionPolicy {
        file_mode: PolicyMode(0o660),
        folder_mode: PolicyMode(0o2770),
        group: Some(100),
    };

    fn seen(rel: &str, folder: bool, mode: u32, uid: u32, gid: u32) -> Seen {
        Seen {
            rel: rel.into(),
            folder,
            id: Identity {
                dev: 1,
                ino: rel.len() as u64,
            },
            mode,
            uid,
            gid,
        }
    }

    #[test]
    fn each_difference_says_who_can_restore_it() {
        let me = Me {
            uid: 4015,
            groups: vec![100],
        };
        let found = compare(
            &POLICY,
            true,
            &me,
            vec![
                seen("", true, 0o2770, 4015, 100),
                seen("a.zip", false, 0o644, 4015, 100),
                seen("b.zip", false, 0o644, 1000, 100),
                seen("same owner elsewhere", false, 0o660, 1000, 100),
                seen("c.zip", false, 0o660, 4015, 5),
                seen("d", true, 0o755, 1000, 5),
            ],
        );
        let causes: Vec<_> = found
            .iter()
            .map(|(s, d)| (s.rel.as_str(), d.cause))
            .collect();
        assert_eq!(
            causes,
            [
                ("a.zip", PermissionCause::Restorable),
                ("b.zip", PermissionCause::OtherOwner),
                ("c.zip", PermissionCause::Restorable),
                ("d", PermissionCause::OtherOwner),
            ]
        );
        let outside = PermissionPolicy {
            group: Some(7),
            ..POLICY
        };
        let found = compare(
            &outside,
            true,
            &me,
            vec![seen("c.zip", false, 0o660, 4015, 5)],
        );
        assert_eq!(found[0].1.cause, PermissionCause::NotInGroup);
        let found = compare(
            &POLICY,
            false,
            &me,
            vec![seen("a.zip", false, 0o644, 4015, 100)],
        );
        assert_eq!(found[0].1.cause, PermissionCause::ReadOnly);
    }

    #[test]
    fn what_stage_select_cannot_restore_comes_with_one_command_for_the_folder() {
        let me = Me {
            uid: 4015,
            groups: vec![100],
        };
        let found: Vec<_> = compare(
            &POLICY,
            true,
            &me,
            vec![
                seen("b.zip", false, 0o644, 1000, 100),
                seen("d", true, 0o755, 1000, 100),
            ],
        )
        .into_iter()
        .map(|(_, d)| d)
        .collect();
        let said = fixes(Path::new("/data/sets/it's"), &POLICY, &me, &found);
        assert_eq!(said.len(), 1);
        assert_eq!(
            said[0].command,
            r"find '/data/sets/it'\''s' -name .stage-select -prune -o \( -type d -o -type f \) -exec chown 4015 {} +"
        );
        assert!(
            said[0].explanation.starts_with(
                "1 file and 1 folder here belong to another user, so Stage Select can't give them the permissions \
                 you chose: 0660 for files and 2770 for folders, in group 100."
            ),
            "{}",
            said[0].explanation
        );
        let read_only: Vec<_> =
            compare(&POLICY, false, &me, vec![seen("a", false, 0o644, 1000, 5)])
                .into_iter()
                .map(|(_, d)| d)
                .collect();
        assert_eq!(
            fixes(Path::new("/s"), &POLICY, &me, &read_only)[0].command,
            r"find '/s' -name .stage-select -prune -o \( -type d -o -type f \) -exec chgrp 100 {} + && find '/s' -name .stage-select -prune -o -type d -exec chmod 2770 {} + -o -type f -exec chmod 0660 {} +"
        );
    }

    #[test]
    fn no_command_for_the_host_reaches_a_stage_select_folder() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let tmp = tempfile::tempdir().unwrap();
        let set = tmp.path().join("it's");
        let state = set.join(".stage-select");
        let nested = set.join("sub/.stage-select");
        for dir in [&state, &nested] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("kept"), b"kept").unwrap();
            std::fs::set_permissions(dir.join("kept"), PermissionsExt::from_mode(0o600)).unwrap();
            std::fs::set_permissions(dir, PermissionsExt::from_mode(0o700)).unwrap();
        }
        std::fs::write(set.join("a.zip"), b"a").unwrap();
        std::fs::set_permissions(set.join("a.zip"), PermissionsExt::from_mode(0o644)).unwrap();
        let me = Me::now();
        let policy = PermissionPolicy {
            file_mode: PolicyMode(0o664),
            folder_mode: PolicyMode(0o775),
            group: Some(rustix::process::getegid().as_raw()),
        };
        let found: Vec<_> = [
            PermissionCause::OtherOwner,
            PermissionCause::NotInGroup,
            PermissionCause::ReadOnly,
        ]
        .into_iter()
        .map(|cause| PermissionDifference {
            path: "a.zip".into(),
            folder: false,
            mode: PolicyMode(0o644),
            uid: me.uid,
            gid: 0,
            expected_mode: PolicyMode(0o664),
            expected_group: policy.group,
            cause,
        })
        .collect();
        let said = fixes(&set, &policy, &me, &found);
        assert_eq!(said.len(), 3);
        for fix in &said {
            let ran = std::process::Command::new("sh")
                .args(["-c", &fix.command])
                .output()
                .unwrap();
            assert!(ran.status.success(), "{}: {ran:?}", fix.command);
        }
        let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().mode() & 0o7777;
        assert_eq!(mode(&set.join("a.zip")), 0o664, "the fix is made");
        for dir in [&state, &nested] {
            assert_eq!(
                (mode(dir), mode(&dir.join("kept"))),
                (0o700, 0o600),
                "{}",
                dir.display()
            );
        }
    }
}
