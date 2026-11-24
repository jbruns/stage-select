//! Creating a missing Root or Source Set folder on request. Each folder is made through the descriptor of the folder
//! above it, opened without following a symlink, so a folder swapped for a symlink after the path was checked can't
//! lead the creation elsewhere. What was made is undone unless the registration it is for is kept.

use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

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
    parent: Rc<OwnedFd>,
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
            let Ok(st) = rustix::fs::statat(&*step.parent, &step.name, AtFlags::SYMLINK_NOFOLLOW)
            else {
                continue;
            };
            if FileType::from_raw_mode(st.st_mode) == FileType::Directory
                && Identity::of(&st) == step.id
            {
                // Removes only an empty folder. Swapped between this check and the removal, another empty folder
                // would go instead: no protected namespace closes that, and only the Operator writes here.
                let _ = rustix::fs::unlinkat(&*step.parent, &step.name, AtFlags::REMOVEDIR);
            }
        }
    }
}

/// A folder held open, pinned: opened from `/` through each component of its canonical path without following a
/// symlink, so a component swapped for a symlink afterwards doesn't move it.
pub(crate) struct Pinned {
    fd: OwnedFd,
    path: PathBuf,
}

impl Pinned {
    /// Pins the folder at `path`, resolving any symlink in it first: [`Pinned::path`] is then its canonical path.
    pub fn open(path: &Path) -> std::io::Result<Pinned> {
        let path = path.canonicalize()?;
        let mut fd = rustix::fs::open("/", DIR_FLAGS, Mode::empty())?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => {
                    fd = rustix::fs::openat(&fd, name, DIR_FLAGS, Mode::empty())?
                }
                _ => return Err(std::io::ErrorKind::InvalidInput.into()),
            }
        }
        Ok(Pinned { fd, path })
    }

    /// The canonical path it was opened at.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Creates the folder at the absolute `path`, and each missing folder above it, as the Service's user, from the
/// deepest folder above it that is there, pinned. `what` names what it is for, as "Root sets", in a refusal.
pub(crate) fn create_folder(path: &Path, what: &str) -> Result<Made> {
    let mut base = path;
    while std::fs::symlink_metadata(base).is_err() {
        match base.parent() {
            Some(up) => base = up,
            None => break,
        }
    }
    let rel = path.strip_prefix(base).unwrap_or(Path::new(""));
    let pinned = Pinned::open(base).map_err(|e| {
        let e = Errno::from_io_error(&e).unwrap_or(Errno::INVAL);
        refusal(what, path)(e, base)
    })?;
    create_beneath(pinned, rel, what, path)
}

/// What a refusal to create `shown` says for an error at `at`.
fn refusal<'a>(what: &'a str, shown: &'a Path) -> impl Fn(Errno, &Path) -> Error + 'a {
    move |e, at| {
        let why = match e {
            Errno::ACCESS | Errno::PERM => {
                "the service's user may not write to the folder above it. Make the \
                 folder yourself, or let the service's user write there."
                    .to_owned()
            }
            Errno::ROFS => "it is on a read-only file system.".to_owned(),
            Errno::EXIST | Errno::NOTDIR => {
                "a file is in the way. Move the file, or check the path.".to_owned()
            }
            Errno::LOOP => format!(
                "{} is a symbolic link, and Stage Select creates a folder only inside real folders. Make the \
                 folder yourself, or check the path.",
                at.display()
            ),
            e => format!("{}: {}.", at.display(), std::io::Error::from(e)),
        };
        Error::Refused(format!(
            "{what}: Stage Select can't create {}: {why}",
            shown.display()
        ))
    }
}

/// Creates the folder `rel` in the pinned folder `base`, and each missing folder between, as [`create_folder`]
/// does, `shown` being the path a refusal shows.
pub(crate) fn create_beneath(base: Pinned, rel: &Path, what: &str, shown: &Path) -> Result<Made> {
    let failed = refusal(what, shown);
    // Opens `name` in `dir` as a folder, a symlink refused as such.
    let open = |dir: &OwnedFd, name: &std::ffi::OsStr, at: &Path| {
        rustix::fs::openat(dir, name, DIR_FLAGS, Mode::empty()).map_err(|e| {
            let symlink = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
                .is_ok_and(|st| FileType::from_raw_mode(st.st_mode) == FileType::Symlink);
            failed(if symlink { Errno::LOOP } else { e }, at)
        })
    };
    let mut made = Made { made: Vec::new() };
    let mut current = Rc::new(base.fd);
    let mut at = base.path;
    for component in rel.components() {
        let Component::Normal(name) = component else {
            return Err(failed(Errno::INVAL, rel));
        };
        at.push(name);
        let opened = match rustix::fs::openat(&*current, name, DIR_FLAGS, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => {
                // The umask applies.
                match rustix::fs::mkdirat(&*current, name, Mode::RWXU | Mode::RWXG | Mode::RWXO) {
                    Ok(()) => {
                        // Recorded before it is opened, so that it is removed even if it can't be.
                        let st = rustix::fs::statat(&*current, name, AtFlags::SYMLINK_NOFOLLOW)
                            .map_err(|e| failed(e, &at))?;
                        made.made.push(Step {
                            parent: Rc::clone(&current),
                            name: name.to_owned(),
                            id: Identity::of(&st),
                        });
                        open(&current, name, &at)?
                    }
                    // Made meanwhile by someone else: opened as any folder that is there.
                    Err(Errno::EXIST) => open(&current, name, &at)?,
                    Err(e) => return Err(failed(e, &at)),
                }
            }
            Err(_) => open(&current, name, &at)?,
        };
        current = Rc::new(opened);
    }
    Ok(made)
}
