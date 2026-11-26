//! Directory handles for Library Folders. A folder is opened once, from its Root's configured path through each
//! component of its relative path without following a symlink, and every read or change in it is then made relative
//! to that handle with the `*at` calls: a folder or staging directory swapped for a symlink after it was checked
//! cannot redirect a write. Files are created exclusively and opened without following a symlink, and a file is
//! renamed into place only by a rename that refuses to replace anything. A file's name may be a path within the
//! folder, such as a disc layout's `<release>.m3u/<file>`: each directory of it is opened without following a symlink.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, Stat};
use stage_select_api::build::{FileStat, STATE};

/// An open directory and the path it was opened at, for messages.
pub(super) struct FolderDir {
    fd: OwnedFd,
    path: PathBuf,
    /// Whether it is the folder's [`STATE`] directory or a folder in it: one Stage Select keeps its own things in.
    own: bool,
    /// Where it is the folder's [`STATE`] directory, or in it, and that was found open to others and not yet closed,
    /// as it is until the folder's marker shows it is this store's ([`FolderDir::close_state`]): what a write in it
    /// is refused with.
    exposed: Option<String>,
}

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn file_mode() -> Mode {
    // The umask applies.
    Mode::RUSR | Mode::WUSR | Mode::RGRP | Mode::WGRP | Mode::ROTH | Mode::WOTH
}

fn dir_mode() -> Mode {
    Mode::RWXU | Mode::RWXG | Mode::RWXO
}

/// A single path component: not empty, `.` or `..`, and holding no `/`.
fn component(name: &str) -> io::Result<&str> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name:?} is not a single file name"),
        ));
    }
    Ok(name)
}

#[cfg(test)]
thread_local! {
    /// Each directory this thread synced, in order.
    pub(crate) static SYNCED: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// What a file or directory is, as opposed to the name it is found at: its device and inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct Identity {
    pub dev: u64,
    pub ino: u64,
}

impl Identity {
    // The field types differ by platform.
    #[allow(clippy::unnecessary_cast)]
    pub fn of(st: &Stat) -> Identity {
        Identity {
            dev: st.st_dev as u64,
            ino: st.st_ino as u64,
        }
    }

    /// The identity of the open `fd`, from `fstat`.
    pub fn of_fd(fd: impl AsFd) -> io::Result<Identity> {
        Ok(Identity::of(&rustix::fs::fstat(fd)?))
    }

    /// As stored in SQLite, which has signed integers only.
    pub fn to_sql(self) -> (i64, i64) {
        (self.dev as i64, self.ino as i64)
    }

    pub fn from_sql(dev: i64, ino: i64) -> Identity {
        Identity {
            dev: dev as u64,
            ino: ino as u64,
        }
    }
}

impl From<Identity> for stage_select_api::build::FileIdentity {
    fn from(id: Identity) -> Self {
        stage_select_api::build::FileIdentity {
            dev: id.dev,
            ino: id.ino,
        }
    }
}

impl From<stage_select_api::build::FileIdentity> for Identity {
    fn from(id: stage_select_api::build::FileIdentity) -> Self {
        Identity {
            dev: id.dev,
            ino: id.ino,
        }
    }
}

/// Whether a rename between the open directories `a` and `b` is proven to work, as both are on one mount. One device
/// alone doesn't prove it: two bind mounts of one filesystem share a device, yet a rename between them fails with
/// `EXDEV`. Where the mount can't be told, a rename isn't proven, and whoever plans for one counts a copy instead.
pub(super) fn one_mount(a: impl AsFd, b: impl AsFd) -> bool {
    mount_of(a.as_fd()).is_some_and(|a| mount_of(b.as_fd()) == Some(a))
}

/// The mount holding `fd`: its mount ID, and the device, from `statx`.
#[cfg(target_os = "linux")]
fn mount_of(fd: std::os::fd::BorrowedFd) -> Option<(u64, u32, u32)> {
    use rustix::fs::StatxFlags;
    let st = rustix::fs::statx(fd, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID).ok()?;
    (st.stx_mask & StatxFlags::MNT_ID.bits() != 0).then_some((
        st.stx_mnt_id,
        st.stx_dev_major,
        st.stx_dev_minor,
    ))
}

/// The mount holding `fd`: where it is mounted, from `fstatfs`, and the device.
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn mount_of(fd: std::os::fd::BorrowedFd) -> Option<(Vec<u8>, u64)> {
    let fs = rustix::fs::fstatfs(fd).ok()?;
    let on: Vec<u8> = fs
        .f_mntonname
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    Some((on, Identity::of_fd(fd).ok()?.dev))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
fn mount_of(_: std::os::fd::BorrowedFd) -> Option<()> {
    None
}

/// A file's status change time in nanoseconds since the Unix epoch: it moves on any change to the file's content or
/// metadata, and cannot be set back.
#[allow(clippy::unnecessary_cast)]
pub(super) fn ctime_ns(st: &Stat) -> i128 {
    st.st_ctime as i128 * 1_000_000_000 + st.st_ctime_nsec as i128
}

/// A file's size and modification time, from `lstat`.
pub(super) fn file_stat(st: &Stat) -> FileStat {
    FileStat {
        size: st.st_size as u64,
        mtime_ns: st.st_mtime as i128 * 1_000_000_000 + st.st_mtime_nsec as i128,
    }
}

/// Why a directory is not one Stage Select keeps its own things in, as [`FolderDir::closed`] finds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Unusable {
    /// It is not what Stage Select made there: not a directory, or not empty. A fragment of a sentence, for its
    /// caller to say what follows.
    Kind(String),
    /// Its permissions or owner keep Stage Select from using it: a whole message, saying what is wrong, what is
    /// expected and the command to run on the Host (ADR 0031). Nothing is to be moved.
    Access(String),
}

impl Unusable {
    /// The message for this: [`Unusable::Access`]'s own, or what `sentence` makes of [`Unusable::Kind`]'s fragment.
    pub fn explain(self, sentence: impl FnOnce(String) -> String) -> String {
        match self {
            Unusable::Kind(why) => sentence(why),
            Unusable::Access(why) => why,
        }
    }
}

/// What Stage Select needs of an entry it keeps its own things in: of a folder, to read, write and search it; of a
/// file, to read and write it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Needs {
    Folder,
    File,
}

impl Needs {
    /// The permission bits of one class that give what is needed, and how `chmod` names them.
    fn bits(self) -> (u32, &'static str) {
        match self {
            Needs::Folder => (0o7, "rwx"),
            Needs::File => (0o6, "rw"),
        }
    }

    /// What the entry is, in a sentence.
    fn noun(self) -> &'static str {
        match self {
            Needs::Folder => "folder",
            Needs::File => "file",
        }
    }
}

/// The owners and groups a test says entries have in place of the ones their filesystems record, by identity: an
/// owner other than the test's own user, or a group it isn't in, which an unprivileged test can't otherwise arrange.
/// Process-wide, as directories are opened with no store at hand; keyed by identity, so each test's entries are its
/// own. Built only with the `test-seams` feature, which only the store's own tests enable.
#[cfg(feature = "test-seams")]
static PRETENDED_OWNERS: std::sync::Mutex<Vec<(Identity, (u32, u32))>> =
    std::sync::Mutex::new(Vec::new());

/// Makes the entry `id` read as owned by the user and group `owner`, or, with `None`, by whoever its filesystem says.
#[cfg(feature = "test-seams")]
pub(super) fn pretend_owner(id: Identity, owner: Option<(u32, u32)>) {
    let mut owners = PRETENDED_OWNERS.lock().unwrap_or_else(|e| e.into_inner());
    owners.retain(|(of, _)| *of != id);
    if let Some(owner) = owner {
        owners.push((id, owner));
    }
}

/// The user and group that own the entry `st` describes: its filesystem's answer, or what a test said.
fn owner_of(st: &Stat) -> (u32, u32) {
    #[cfg(feature = "test-seams")]
    {
        let id = Identity::of(st);
        if let Some((_, owner)) = PRETENDED_OWNERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|(of, _)| *of == id)
        {
            return *owner;
        }
    }
    (st.st_uid, st.st_gid)
}

/// What Stage Select changed of the permissions of its own entries, by the entry's path, in sentences, until the
/// next plan of the folder holding it says so. Process-wide, as a scan or check on any thread may make the change
/// that the folder's next plan reports; kept until then, or until the service stops.
static REPAIRED: std::sync::Mutex<Vec<(PathBuf, String)>> = std::sync::Mutex::new(Vec::new());

/// What Stage Select changed of the permissions of its own entries in the folder at `folder` since a plan of it last
/// said so, in sentences: for a plan of it to say.
pub(super) fn take_repaired(folder: &Path) -> Vec<String> {
    let mut repaired = REPAIRED.lock().unwrap_or_else(|e| e.into_inner());
    let (taken, kept) = std::mem::take(&mut *repaired)
        .into_iter()
        .partition(|(path, _)| path.starts_with(folder));
    *repaired = kept;
    taken.into_iter().map(|(_, said)| said).collect()
}

/// `path` quoted for a POSIX shell.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// What Stage Select could open of an entry it checks with [`closed_to_others`].
#[derive(Clone, Copy)]
pub(super) enum Opened<'a> {
    /// The folder, open for reading.
    Folder(std::os::fd::BorrowedFd<'a>),
    /// The file, open for reading and writing.
    File(std::os::fd::BorrowedFd<'a>),
    /// Nothing: opening it was refused.
    Not,
}

impl Opened<'_> {
    /// Whether the kernel lets Stage Select use the entry as `needs` says, whatever grants or denies it: its mode,
    /// an access control list, or the mount.
    fn kernel_allows(self) -> bool {
        match self {
            Opened::Folder(fd) => {
                use rustix::fs::Access;
                // Only a refusal counts as one: where the call itself is refused, as a sandbox may refuse
                // `faccessat2`, the mode decides alone.
                !matches!(
                    rustix::fs::accessat(
                        fd,
                        ".",
                        Access::READ_OK | Access::WRITE_OK | Access::EXEC_OK,
                        AtFlags::EACCESS,
                    ),
                    Err(rustix::io::Errno::ACCESS | rustix::io::Errno::ROFS)
                )
            }
            // Opened for reading and writing, so it is allowed.
            Opened::File(_) => true,
            Opened::Not => false,
        }
    }
}

/// Whether [`closed_to_others`] may remove others' access to an entry Stage Select owns: only once the entry is known
/// to be this store's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PermissionFix {
    Allowed,
    Deferred,
}

/// How [`closed_to_others`] found an entry Stage Select may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Standing {
    /// No one beyond its owner and group has access to it.
    Closed,
    /// Others have access to it, and Stage Select, which owns it, would remove that access once the entry is known to
    /// be this store's ([`PermissionFix::Deferred`]). Until then nothing is written in it: this is what a write is refused
    /// with.
    Exposed(String),
}

/// The sentence a repair is reported by: the permissions before, and the ones read back after, saying so if the
/// filesystem kept setgid off.
pub(super) fn repair_note(path: &Path, was: u32, asked: u32, now: u32) -> String {
    let mut note = format!(
        "Others had access to {} (its permissions were {was:o}), so Stage Select removed their access: its \
         permissions are now {now:o}.",
        path.display()
    );
    if asked & 0o2000 != 0 && now & 0o2000 == 0 {
        note.push_str(
            " The filesystem cleared its setgid bit, so new entries in it no longer take its group; set it again \
             with chmod g+s if you need that.",
        );
    }
    note
}

/// Whether Stage Select may keep its own things in the entry at `path`, described by `st` and open as `opened`
/// says: whether it can use it as `needs` says and others, beyond its owner and group, have no access to it (ADR
/// 0031). Group bits and setgid are allowed, whoever owns it. Where Stage Select owns it, can use it and others have
/// access, that access is removed through the open descriptor under [`PermissionFix::Allowed`], its other bits kept, and
/// [`take_repaired`] says so with the permissions read back; under [`PermissionFix::Deferred`] it is left and
/// [`Standing::Exposed`] returned. Otherwise returns what is wrong, what is expected and the command to run.
///
/// Whether Stage Select can use it is the kernel's answer, through the descriptor, as well as the mode's for the
/// class that applies to it: an access control list or a mount that keeps it out is refused, though no command
/// fixes it. Whether others have access is read from the mode's "other" bits alone: an access control list entry
/// granting another user access is not looked for.
// `st_mode` is a `u16` on macOS and a `u32` on Linux.
#[allow(clippy::unnecessary_cast)]
pub(super) fn closed_to_others(
    opened: Opened,
    st: &Stat,
    path: &Path,
    needs: Needs,
    fix: PermissionFix,
) -> std::result::Result<Standing, String> {
    let mode = st.st_mode as u32 & 0o7777;
    let (owner, gid) = owner_of(st);
    let uid = rustix::process::geteuid().as_raw();
    let in_group = rustix::process::getegid().as_raw() == gid
        || rustix::process::getgroups()
            .is_ok_and(|groups| groups.iter().any(|g| g.as_raw() == gid));
    let (want, _) = needs.bits();
    let ours = owner == uid;
    // Only the class that applies to Stage Select counts: the owner's if it is the owner, else the group's.
    let mode_allows = uid == 0
        || if ours {
            (mode >> 6) & want == want
        } else {
            in_group && (mode >> 3) & want == want
        };
    let kernel_allows = opened.kernel_allows();
    let usable = mode_allows && kernel_allows;
    let open = mode & 0o007 != 0;
    if usable && !open {
        return Ok(Standing::Closed);
    }
    let refusal = || {
        refusal(
            path,
            mode,
            (owner, gid, uid),
            needs,
            (ours, in_group),
            (mode_allows, kernel_allows),
        )
    };
    if usable && ours {
        let fd = match opened {
            Opened::Folder(fd) | Opened::File(fd) => fd,
            Opened::Not => return Err(refusal()),
        };
        if fix == PermissionFix::Deferred {
            return Ok(Standing::Exposed(refusal()));
        }
        let closed = mode & !0o007;
        // Where the change fails, as on a read-only mount, it is refused with the command that fixes it.
        if rustix::fs::fchmod(fd, Mode::from_raw_mode(closed as _)).is_ok()
            && let Ok(after) = rustix::fs::fstat(fd)
        {
            let now = after.st_mode as u32 & 0o7777;
            if now & 0o007 == 0 {
                REPAIRED
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((path.to_path_buf(), repair_note(path, mode, closed, now)));
                return Ok(Standing::Closed);
            }
        }
    }
    Err(refusal())
}

/// What [`closed_to_others`] refuses an entry with: what is wrong, what is expected, and the command that fixes it.
/// `ids` are its owner, its group and Stage Select's user; `standing` whether Stage Select owns it and is in its
/// group; `allows` whether its mode and the kernel let Stage Select use it.
fn refusal(
    path: &Path,
    mode: u32,
    (owner, gid, uid): (u32, u32, u32),
    needs: Needs,
    (ours, in_group): (bool, bool),
    (mode_allows, kernel_allows): (bool, bool),
) -> String {
    let (want, class) = needs.bits();
    let usable = mode_allows && kernel_allows;
    let open = mode & 0o007 != 0;
    let kind = needs.noun();
    let mut wrong = Vec::new();
    if !usable {
        wrong.push(if mode_allows {
            "Stage Select can't read and write it, though its permissions allow that".to_owned()
        } else {
            "Stage Select can't read and write it".to_owned()
        });
    }
    if open {
        wrong.push("others have access to it".to_owned());
    }
    let at = quoted(path);
    let (command, who) = if usable || ours || in_group {
        let mut changes = Vec::new();
        if !mode_allows {
            changes.push(format!("{}+{class}", if ours { 'u' } else { 'g' }));
        }
        if open {
            changes.push("o-rwx".to_owned());
        }
        (
            (!changes.is_empty()).then(|| format!("chmod {} {at}", changes.join(","))),
            "its owner or as root",
        )
    } else {
        let mut changes = Vec::new();
        if (mode >> 6) & want != want {
            changes.push(format!("u+{class}"));
        }
        if open {
            changes.push("o-rwx".to_owned());
        }
        let chown = format!("chown {uid} {at}");
        (
            Some(if changes.is_empty() {
                chown
            } else {
                format!("{chown} && chmod {} {at}", changes.join(","))
            }),
            "root",
        )
    };
    let what = format!(
        "{} can't be used: {} (it is owned by uid {owner} and group {gid}, with permissions {mode:o}). Stage \
         Select keeps its own things in this {kind}, so it needs to read and write it, and no one beyond its owner \
         and group may have access to it.",
        path.display(),
        wrong.join(" and "),
    );
    let after = if !usable && mode_allows {
        // The mode allows it, so what keeps Stage Select out is an access control list or the mount, which no chmod
        // changes.
        " An access control list or the mount keeps Stage Select out."
    } else {
        ""
    };
    match command {
        Some(command) => format!(
            "{what}{after} To fix it, run this on the host as {who}, then try again (the path is as Stage Select sees \
             it: if Stage Select runs in a container, use the host folder mounted there): {command}"
        ),
        None => format!(
            "{what}{after} To fix it, let Stage Select read and write it on the host, as {who}, then try again."
        ),
    }
}

impl FolderDir {
    /// Opens the folder `rel` (`/`-separated; empty for the Root itself) in the Root at `root`. The Root's own path is followed as configured;
    /// no component of `rel` may be a symlink.
    pub fn open(root: &Path, rel: &str) -> io::Result<FolderDir> {
        let fd = rustix::fs::open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| with_path(root, e.into()))?;
        let mut dir = FolderDir {
            fd,
            path: root.to_path_buf(),
            own: false,
            exposed: None,
        };
        if !rel.is_empty() {
            for part in rel.split('/') {
                dir = dir.sub(part)?;
            }
        }
        Ok(dir)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn at(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    /// The directory holding `rel`, a `/`-separated path relative to this one, opened through each component
    /// without following a symlink (`None` for this one itself), and its last component.
    fn resolve<'a>(&self, rel: &'a str) -> io::Result<(Option<FolderDir>, &'a str)> {
        let (owned, name) = parent_of(self, rel)?;
        Ok((owned, component(name)?))
    }

    /// The directory `name` in this one, a `/`-separated path, no component of it followed if it is a symlink.
    /// The folder's [`STATE`] directory, and so any path through it, opens only while Stage Select can use it
    /// ([`closed_to_others`]). One open to others is opened for reading only, and nothing is written in it until
    /// [`FolderDir::close_state`] closes it: anyone else with access could otherwise replace the marker in it, or move
    /// a folder Stage Select writes into out of the folder.
    pub fn sub(&self, name: &str) -> io::Result<FolderDir> {
        let (parent, leaf) = self.resolve(name)?;
        let state = parent.is_none() && leaf == STATE;
        let holder = parent.as_ref().unwrap_or(self);
        let own = state || holder.own;
        let fd = rustix::fs::openat(&holder.fd, leaf, DIR_FLAGS, Mode::empty()).map_err(|e| {
            match e {
                rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP if state => {
                    self.not_state("it is not a directory")
                }
                // One of Stage Select's own folders it can't even open is explained as any other it can't use.
                rustix::io::Errno::ACCESS if own => {
                    match rustix::fs::statat(&holder.fd, leaf, AtFlags::SYMLINK_NOFOLLOW) {
                        Ok(st) if FolderDir::kind(&st) == FileType::Directory => {
                            match closed_to_others(
                                Opened::Not,
                                &st,
                                &self.at(name),
                                Needs::Folder,
                                PermissionFix::Deferred,
                            ) {
                                Err(why) => io::Error::new(io::ErrorKind::PermissionDenied, why),
                                Ok(_) => self.error(name, e),
                            }
                        }
                        _ => self.error(name, e),
                    }
                }
                e => self.error(name, e),
            }
        })?;
        let mut found = FolderDir {
            fd,
            path: self.at(name),
            own,
            exposed: holder.exposed.clone(),
        };
        if state {
            // Others' access is not removed here: this may be another store's folder, as a backup of one is, until its
            // marker shows it is this store's.
            match found.standing(PermissionFix::Deferred)? {
                Ok(Standing::Closed) => {}
                Ok(Standing::Exposed(why)) => found.exposed = Some(why),
                Err(why) => return Err(self.unusable_state(why)),
            }
        }
        Ok(found)
    }

    /// Closes the folder's [`STATE`] directory to others, where Stage Select owns it and others have access, once
    /// its marker, read through the descriptor the change is made through, names the store `store`; a folder whose
    /// marker names another store, or has none, is left as it is. Returns why it could not be closed.
    pub fn close_state(&self, store: &str) -> std::result::Result<(), String> {
        #[derive(serde::Deserialize)]
        struct Of {
            store: String,
        }
        let Ok(fd) = rustix::fs::openat(&self.fd, STATE, DIR_FLAGS, Mode::empty()) else {
            return Ok(());
        };
        let marker = stage_select_api::build::MARKER
            .strip_prefix(&format!("{STATE}/"))
            .unwrap_or_default();
        let ours = rustix::fs::openat(
            &fd,
            marker,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .ok()
        .and_then(|file| {
            let mut bytes = Vec::new();
            File::from(file)
                .take(64 << 10)
                .read_to_end(&mut bytes)
                .ok()?;
            serde_json::from_slice::<Of>(&bytes).ok()
        })
        .is_some_and(|of| of.store == store);
        if !ours {
            return Ok(());
        }
        let st = rustix::fs::fstat(&fd).map_err(|e| e.to_string())?;
        if FolderDir::kind(&st) != FileType::Directory {
            return Ok(());
        }
        closed_to_others(
            Opened::Folder(fd.as_fd()),
            &st,
            &self.at(STATE),
            Needs::Folder,
            PermissionFix::Allowed,
        )
        .map(|_| ())
    }

    /// What a change in the folder's [`STATE`] directory is refused with while it is open to others and not yet
    /// closed; `None` when it is closed, or absent.
    pub fn state_exposed(&self) -> io::Result<Option<String>> {
        match self.sub(STATE) {
            Ok(state) => Ok(state.exposed),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Refuses a change in the directory `holder`, the one this directory's path resolves through, while it is the
    /// folder's [`STATE`] directory, or in it, open to others and not yet closed.
    fn writable(holder: &FolderDir) -> io::Result<()> {
        match &holder.exposed {
            Some(why) => Err(io::Error::new(io::ErrorKind::PermissionDenied, why.clone())),
            None => Ok(()),
        }
    }

    /// Why the folder's [`STATE`] directory, found as `why` says, is never used: it is not a directory at all.
    fn not_state(&self, why: &str) -> io::Error {
        io::Error::other(format!(
            "{} is not Stage Select's own folder: {why}; it was left as it is. Move it out of the folder, then try \
             again",
            self.at(STATE).display()
        ))
    }

    /// Why the folder's [`STATE`] directory is not used now, as [`FolderDir::closed`] found.
    fn unusable_state(&self, why: Unusable) -> io::Error {
        io::Error::other(why.explain(|why| self.not_state(&why).to_string()))
    }

    /// Creates the directory `name`, failing if anything is there.
    pub fn mkdir(&self, name: &str) -> io::Result<()> {
        let name = component(name)?;
        FolderDir::writable(self)?;
        rustix::fs::mkdirat(&self.fd, name, dir_mode()).map_err(|e| self.error(name, e))
    }

    /// Creates the directory `name` for this service's user only, failing if anything is there.
    pub fn mkdir_private(&self, name: &str) -> io::Result<()> {
        let name = component(name)?;
        FolderDir::writable(self)?;
        rustix::fs::mkdirat(&self.fd, name, Mode::RWXU).map_err(|e| self.error(name, e))
    }

    /// Checks, through the open descriptor, that this is a directory Stage Select may keep its own things in: a
    /// directory it can read and write that others, beyond its owner and group, have no access to, whoever owns it
    /// (ADR 0031); and, with `empty`, one holding nothing. One Stage Select owns that others have access to has that
    /// access removed here, without a plan, and [`take_repaired`] says so. Returns its identity, or why it is not.
    ///
    /// A process running as the service's user, or in the folder's group, can rewrite the store's folders as well, so
    /// it is not defended against. What this rejects is a directory anyone else could change, or one Stage Select
    /// can't use; [`Unusable::Access`] says how to fix it on the Host.
    pub fn closed(&self, empty: bool) -> io::Result<std::result::Result<Identity, Unusable>> {
        match self.standing(PermissionFix::Allowed)? {
            Ok(_) => {}
            Err(why) => return Ok(Err(why)),
        }
        if empty && !self.entries()?.is_empty() {
            return Ok(Err(Unusable::Kind("it is not empty".into())));
        }
        self.identity().map(Ok)
    }

    /// Whether this is a directory Stage Select may keep its own things in, as [`closed_to_others`] finds under
    /// `fix`.
    fn standing(&self, fix: PermissionFix) -> io::Result<std::result::Result<Standing, Unusable>> {
        let st = rustix::fs::fstat(&self.fd).map_err(|e| with_path(&self.path, e.into()))?;
        if FolderDir::kind(&st) != FileType::Directory {
            return Ok(Err(Unusable::Kind("it is not a directory".into())));
        }
        Ok(closed_to_others(
            Opened::Folder(self.fd.as_fd()),
            &st,
            &self.path,
            Needs::Folder,
            fix,
        )
        .map_err(Unusable::Access))
    }

    /// This same open directory, on a descriptor of its own.
    pub fn try_clone(&self) -> io::Result<FolderDir> {
        Ok(FolderDir {
            fd: self.fd.try_clone()?,
            path: self.path.clone(),
            own: self.own,
            exposed: self.exposed.clone(),
        })
    }

    /// The identity of this directory as it was opened.
    pub fn identity(&self) -> io::Result<Identity> {
        Identity::of_fd(&self.fd).map_err(|e| with_path(&self.path, e))
    }

    /// Creates the new file `name` for reading and writing, failing if anything, a symlink included, is there.
    pub fn create_new(&self, name: &str) -> io::Result<File> {
        let (parent, leaf) = self.resolve(name)?;
        FolderDir::writable(parent.as_ref().unwrap_or(self))?;
        let flags =
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(
            &parent.as_ref().unwrap_or(self).fd,
            leaf,
            flags,
            file_mode(),
        )
        .map_err(|e| self.error(name, e))?;
        Ok(File::from(fd))
    }

    /// Opens the regular file `name` for reading and writing, refusing a symlink or anything else.
    pub fn open_write(&self, name: &str) -> io::Result<File> {
        let (parent, leaf) = self.resolve(name)?;
        FolderDir::writable(parent.as_ref().unwrap_or(self))?;
        let flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(
            &parent.as_ref().unwrap_or(self).fd,
            leaf,
            flags,
            Mode::empty(),
        )
        .map_err(|e| self.error(name, e))?;
        if FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode) != FileType::RegularFile {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", self.at(name).display()),
            ));
        }
        Ok(File::from(fd))
    }

    /// Opens the regular file `name` for reading, refusing a symlink or anything else.
    pub fn open_file(&self, name: &str) -> io::Result<File> {
        let (parent, leaf) = self.resolve(name)?;
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(
            &parent.as_ref().unwrap_or(self).fd,
            leaf,
            flags,
            Mode::empty(),
        )
        .map_err(|e| self.error(name, e))?;
        if FileType::from_raw_mode(rustix::fs::fstat(&fd)?.st_mode) != FileType::RegularFile {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", self.at(name).display()),
            ));
        }
        Ok(File::from(fd))
    }

    /// Holds `name` open while it is the regular file `id`, so that while a rename moves it and the result is
    /// checked, its inode number can't be freed and reused by a file put in its place; `None` when it isn't.
    pub fn pin(&self, name: &str, id: Identity) -> Option<File> {
        let file = self.open_file(name).ok()?;
        (Identity::of_fd(&file).ok()? == id).then_some(file)
    }

    /// The `lstat` of `name`, or `None` if nothing is there.
    pub fn stat(&self, name: &str) -> io::Result<Option<Stat>> {
        let (parent, leaf) = match self.resolve(name) {
            Ok(found) => found,
            // A folder of the path that is not there holds nothing.
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        match rustix::fs::statat(
            &parent.as_ref().unwrap_or(self).fd,
            leaf,
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Ok(st) => Ok(Some(st)),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(e) => Err(self.error(name, e)),
        }
    }

    pub fn kind(st: &Stat) -> FileType {
        FileType::from_raw_mode(st.st_mode)
    }

    /// Renames `from` in this directory to `to` in `dest`, failing with `AlreadyExists` rather than replacing
    /// anything at `to`. There is no fallback: where the filesystem cannot rename exclusively, it fails.
    pub fn rename_new(&self, from: &str, dest: &FolderDir, to: &str) -> io::Result<()> {
        let (from_parent, from_leaf) = self.resolve(from)?;
        let (to_parent, to_leaf) = dest.resolve(to)?;
        FolderDir::writable(from_parent.as_ref().unwrap_or(self))?;
        FolderDir::writable(to_parent.as_ref().unwrap_or(dest))?;
        match rustix::fs::renameat_with(
            &from_parent.as_ref().unwrap_or(self).fd,
            from_leaf,
            &to_parent.as_ref().unwrap_or(dest).fd,
            to_leaf,
            RenameFlags::NOREPLACE,
        ) {
            Ok(()) => Ok(()),
            Err(rustix::io::Errno::EXIST) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", dest.at(to).display()),
            )),
            Err(
                e @ (rustix::io::Errno::INVAL
                | rustix::io::Errno::NOSYS
                | rustix::io::Errno::NOTSUP),
            ) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "{}: the filesystem cannot rename a file without replacing what is there ({e}), so nothing \
                     is built into it",
                    dest.path.display()
                ),
            )),
            Err(e) => Err(dest.error(to, e)),
        }
    }

    /// Renames `from` over `to`, both in this directory, replacing `to` itself if it is there.
    pub fn replace(&self, from: &str, to: &str) -> io::Result<()> {
        let (from_parent, from_leaf) = self.resolve(from)?;
        let (to_parent, to_leaf) = self.resolve(to)?;
        FolderDir::writable(from_parent.as_ref().unwrap_or(self))?;
        FolderDir::writable(to_parent.as_ref().unwrap_or(self))?;
        rustix::fs::renameat(
            &from_parent.as_ref().unwrap_or(self).fd,
            from_leaf,
            &to_parent.as_ref().unwrap_or(self).fd,
            to_leaf,
        )
        .map_err(|e| self.error(to, e))
    }

    /// Removes the file or symlink `name`; a symlink's target is never touched.
    pub fn unlink(&self, name: &str) -> io::Result<()> {
        let (parent, leaf) = self.resolve(name)?;
        FolderDir::writable(parent.as_ref().unwrap_or(self))?;
        rustix::fs::unlinkat(&parent.as_ref().unwrap_or(self).fd, leaf, AtFlags::empty())
            .map_err(|e| self.error(name, e))
    }

    /// Removes the empty directory `name`.
    pub fn rmdir(&self, name: &str) -> io::Result<()> {
        let (parent, leaf) = self.resolve(name)?;
        FolderDir::writable(parent.as_ref().unwrap_or(self))?;
        rustix::fs::unlinkat(
            &parent.as_ref().unwrap_or(self).fd,
            leaf,
            AtFlags::REMOVEDIR,
        )
        .map_err(|e| self.error(name, e))
    }

    /// Makes the entries of the directory holding `rel`, a `/`-separated path relative to this one, durable: this
    /// directory's for a single name, and for a name in [`STATE`](stage_select_api::build::STATE), that directory's.
    pub fn sync_holding(&self, rel: &str) -> io::Result<()> {
        match self.resolve(rel)? {
            (Some(parent), _) => parent.sync(),
            (None, _) => self.sync(),
        }
    }

    /// Makes the directory's entries durable.
    pub fn sync(&self) -> io::Result<()> {
        #[cfg(test)]
        SYNCED.with(|synced| synced.borrow_mut().push(self.path.clone()));
        rustix::fs::fsync(&self.fd).map_err(|e| with_path(&self.path, e.into()))
    }

    /// The names in the directory, other than `.` and `..`, with their types as the listing gives them.
    pub fn entries(&self) -> io::Result<Vec<(String, FileType)>> {
        let mut dir = Dir::read_from(&self.fd).map_err(|e| with_path(&self.path, e.into()))?;
        let mut names = Vec::new();
        while let Some(entry) = dir.read() {
            let entry = entry.map_err(|e| with_path(&self.path, e.into()))?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            names.push((
                OsStr::from_bytes(bytes).to_string_lossy().into_owned(),
                entry.file_type(),
            ));
        }
        Ok(names)
    }

    /// Reads the regular file `name`, up to `limit` bytes.
    pub fn read(&self, name: &str, limit: u64) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.open_file(name)?
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| with_path(&self.at(name), e))?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is larger than {limit} bytes", self.at(name).display()),
            ));
        }
        Ok(bytes)
    }

    fn error(&self, name: &str, e: rustix::io::Errno) -> io::Error {
        with_path(&self.at(name), e.into())
    }
}

/// Opens the folder holding `rel`, a path relative to `dir`, without following a symlink: `None` for `dir` itself.
/// Returns it with the last component.
pub(super) fn parent_of<'a>(
    dir: &FolderDir,
    rel: &'a str,
) -> io::Result<(Option<FolderDir>, &'a str)> {
    let Some((parents, name)) = rel.rsplit_once('/') else {
        return Ok((None, rel));
    };
    let mut parts = parents.split('/');
    let mut found = dir.sub(parts.next().unwrap_or_default())?;
    for part in parts {
        found = found.sub(part)?;
    }
    Ok((Some(found), name))
}

/// Opens the folder holding `rel`, a path relative to `dir`, as [`parent_of`] does, making each folder of it that
/// is missing first: a loose folder a repair writes a Game into, or a
/// folder of a disc layout a Build publishes into.
pub(super) fn parent_made<'a>(
    dir: &FolderDir,
    rel: &'a str,
) -> io::Result<(Option<FolderDir>, &'a str)> {
    let Some((parents, name)) = rel.rsplit_once('/') else {
        return Ok((None, rel));
    };
    let mut found: Option<FolderDir> = None;
    for part in parents.split('/') {
        let at = found.as_ref().unwrap_or(dir);
        // The folder's own STATE directory is made private to this service's user, and used only while it is closed
        // to others.
        let state = found.is_none() && part == STATE;
        let made = match if state {
            at.mkdir_private(part)
        } else {
            at.mkdir(part)
        } {
            Ok(()) => true,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e),
        };
        let sub = at.sub(part)?;
        if made
            && state
            && let Err(why) = sub.closed(true)?
        {
            return Err(at.unusable_state(why));
        }
        // Synced even when it already existed: a stop after making it may have left it unsynced, and what is
        // recorded inside it is durable only once it is.
        at.sync()?;
        found = Some(sub);
    }
    Ok((found, name))
}

/// The name, in a folder's [`STATE`] directory, that a new file meant for `name` is written under before one rename
/// that never replaces anything puts it in place: on the folder's own filesystem, and never a name Stage Select
/// gives anything else there.
pub(super) fn staged_name(name: &str) -> String {
    format!("{STATE}/new-{}-{name}", super::kept::unique())
}

/// Creates the new file `name` in `dir` as [`FolderDir::create_new`] does, first making each folder of it that is
/// missing: the [`STATE`] directory a [`staged_name`] is in.
pub(super) fn create_staged(dir: &FolderDir, name: &str) -> io::Result<File> {
    parent_made(dir, name)?;
    dir.create_new(name)
}

/// Removes the folder holding `rel`, a path relative to `dir`, if it is now empty: a loose folder a repair emptied, or a folder
/// of a disc layout.
/// Best effort; a folder still holding anything stays.
pub(super) fn remove_emptied(dir: &FolderDir, rel: &str) {
    if let Some((parents, _)) = rel.rsplit_once('/')
        && let Ok((parent, name)) = parent_of(dir, parents)
    {
        let parent = parent.as_ref().unwrap_or(dir);
        if parent.rmdir(name).is_ok() {
            let _ = parent.sync();
        }
    }
}

impl AsFd for FolderDir {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

fn with_path(path: &Path, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_folder_made_before_a_stop_is_synced_into_its_folder_when_next_used() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = FolderDir::open(tmp.path(), "").unwrap();
        // A stop between making `.stage-select` and syncing the folder holding it.
        dir.mkdir_private(STATE).unwrap();
        SYNCED.with(|synced| synced.borrow_mut().clear());

        parent_made(&dir, &format!("{STATE}/marker")).unwrap();

        let synced = SYNCED.with(|synced| synced.borrow().clone());
        assert!(synced.contains(&tmp.path().to_path_buf()), "{synced:?}");
    }

    #[test]
    fn a_repair_says_the_permissions_read_back_and_a_setgid_bit_the_filesystem_cleared() {
        let path = Path::new("/library/.stage-select");
        assert_eq!(
            repair_note(path, 0o2777, 0o2770, 0o2770),
            "Others had access to /library/.stage-select (its permissions were 2777), so Stage Select removed their \
             access: its permissions are now 2770."
        );
        assert_eq!(
            repair_note(path, 0o2777, 0o2770, 0o770),
            "Others had access to /library/.stage-select (its permissions were 2777), so Stage Select removed their \
             access: its permissions are now 770. The filesystem cleared its setgid bit, so new entries in it no \
             longer take its group; set it again with chmod g+s if you need that."
        );
    }

    /// Two folders of one mount are proven to rename between; a folder and one on another mount, `/dev`, are not.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "freebsd"))]
    #[test]
    fn a_rename_is_proven_only_within_one_mount() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("a/deep")).unwrap();
        std::fs::create_dir(tmp.path().join("b")).unwrap();
        let a = FolderDir::open(tmp.path(), "a/deep").unwrap();
        let b = FolderDir::open(tmp.path(), "b").unwrap();
        assert!(one_mount(&a, &b));
        let dev = FolderDir::open(Path::new("/dev"), "").unwrap();
        assert!(!one_mount(&a, &dev));
    }
}
