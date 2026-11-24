//! Creating a missing Root or Source Set folder on request. Each folder is made through the descriptor of the folder
//! above it, opened without following a symlink, so a folder swapped for a symlink after the path was checked can't
//! lead the creation elsewhere. What was made is undone unless the registration it is for is kept.

use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::path::{Component, Path};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;

use crate::folder_dir::Identity;
use crate::{Error, Result};

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// The folders one creation made, shallowest first. Dropped without [`Made::keep`], it removes each, deepest first,
/// if it is still that folder and still empty: never a folder that was there before.
#[must_use]
pub(crate) struct Made {
    made: Vec<Step>,
}

/// A folder made: the folder above it, held open, its name there, and what it is.
struct Step {
    parent: OwnedFd,
    name: OsString,
    id: Identity,
}

impl Made {
    /// Whether it made any folder.
    pub fn is_empty(&self) -> bool {
        self.made.is_empty()
    }

    /// Keeps what it made.
    pub fn keep(mut self) {
        self.made.clear();
    }
}

impl Drop for Made {
    fn drop(&mut self) {
        for step in self.made.drain(..).rev() {
            let Ok(st) = rustix::fs::statat(&step.parent, &step.name, AtFlags::SYMLINK_NOFOLLOW)
            else {
                continue;
            };
            if FileType::from_raw_mode(st.st_mode) == FileType::Directory
                && Identity::of(&st) == step.id
            {
                // Removes only an empty folder.
                let _ = rustix::fs::unlinkat(&step.parent, &step.name, AtFlags::REMOVEDIR);
            }
        }
    }
}

/// Creates the folder at the absolute `path`, and each missing folder above it, as the Service's user, from the
/// deepest folder above it that is there. `what` names what it is for, as "Root sets", in a refusal.
pub(crate) fn create_folder(path: &Path, what: &str) -> Result<Made> {
    let mut base = path;
    while std::fs::symlink_metadata(base).is_err() {
        match base.parent() {
            Some(up) => base = up,
            None => break,
        }
    }
    let rel = path.strip_prefix(base).unwrap_or(Path::new(""));
    create_beneath(base, rel, what, path)
}

/// Creates the folder `rel` in the folder `base`, and each missing folder between, as [`create_folder`] does,
/// `shown` being the path a refusal shows.
pub(crate) fn create_beneath(base: &Path, rel: &Path, what: &str, shown: &Path) -> Result<Made> {
    let refuse = |why: String| {
        Error::Refused(format!(
            "{what}: Stage Select can't create {}: {why}",
            shown.display()
        ))
    };
    let failed = |e: Errno, at: &Path| {
        match e {
        Errno::ACCESS | Errno::PERM => refuse(
            "the service's user may not write to the folder above it. Make the folder yourself, or let the \
             service's user write there."
                .into(),
        ),
        Errno::ROFS => refuse("it is on a read-only file system.".into()),
        Errno::EXIST | Errno::NOTDIR => {
            refuse("a file is in the way. Move the file, or check the path.".into())
        }
        e => refuse(format!("{}: {}.", at.display(), std::io::Error::from(e))),
    }
    };
    let mut made = Made { made: Vec::new() };
    let mut current = rustix::fs::open(
        base,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| failed(e, base))?;
    let mut at = base.to_path_buf();
    for component in rel.components() {
        let Component::Normal(name) = component else {
            return Err(refuse(format!("{} is not a plain path.", rel.display())));
        };
        at.push(name);
        let opened = match rustix::fs::openat(&current, name, DIR_FLAGS, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => {
                match rustix::fs::mkdirat(&current, name, Mode::RWXU | Mode::RWXG | Mode::RWXO) {
                    // The umask applies.
                    Ok(()) => {
                        let child = rustix::fs::openat(&current, name, DIR_FLAGS, Mode::empty())
                            .map_err(|e| not_folder(&current, name, e, &at, &refuse, &failed))?;
                        let id = Identity::of_fd(&child)
                            .map_err(|e| refuse(format!("{}: {e}.", at.display())))?;
                        made.made.push(Step {
                            parent: current,
                            name: name.to_owned(),
                            id,
                        });
                        current = child;
                        continue;
                    }
                    // Made meanwhile by someone else: opened as any folder that is there.
                    Err(Errno::EXIST) => {
                        rustix::fs::openat(&current, name, DIR_FLAGS, Mode::empty())
                            .map_err(|e| not_folder(&current, name, e, &at, &refuse, &failed))?
                    }
                    Err(e) => return Err(failed(e, &at)),
                }
            }
            Err(e) => return Err(not_folder(&current, name, e, &at, &refuse, &failed)),
        };
        current = opened;
    }
    Ok(made)
}

/// Why `name` in `dir` couldn't be opened as a folder: a symlink, or `e` as it is.
fn not_folder(
    dir: &OwnedFd,
    name: &std::ffi::OsStr,
    e: Errno,
    at: &Path,
    refuse: &impl Fn(String) -> Error,
    failed: &impl Fn(Errno, &Path) -> Error,
) -> Error {
    let symlink = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
        .is_ok_and(|st| FileType::from_raw_mode(st.st_mode) == FileType::Symlink);
    if symlink {
        refuse(format!(
            "{} is a symbolic link, and Stage Select creates a folder only inside real folders. Make the folder \
             yourself, or check the path.",
            at.display()
        ))
    } else {
        failed(e, at)
    }
}
