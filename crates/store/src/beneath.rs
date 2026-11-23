//! Resolving a Source Set's or a Library Folder's path beneath its Root: the one place that checks such a path and
//! canonicalizes the Root it is resolved against.
//!
//! Both kinds of path follow symlinks here, as the Host would. A Source Set's folder must already exist and resolve
//! inside the Root; a Library Folder's need not exist yet, and is only resolved, to be checked for overlap, as
//! opening it never follows a symlink ([`crate::folder_dir::FolderDir`]).

use std::path::{Component, Path, PathBuf};

use stage_select_api::paths::relative_path;

use crate::roots::Root;
use crate::{Error, Result};

/// A path checked to be a plain relative path beneath a Root, with the Root's canonical path.
pub(crate) struct Beneath<'a> {
    root: &'a Root,
    /// Relative to the Root, with `/` separators, without empty or `.` components.
    pub rel: String,
    /// The Root's canonical path.
    pub canonical_root: PathBuf,
}

/// Checks that `rel` is a plain relative path, naming a folder, and canonicalizes `root`'s path. Refuses an empty
/// path, an absolute one, `..` and `\`. `what` names the path in a refusal, as in "a source set's path".
pub(crate) fn resolve_beneath<'a>(root: &'a Root, rel: &str, what: &str) -> Result<Beneath<'a>> {
    let rel = relative_path(rel).map_err(|e| Error::Refused(format!("{rel}: {what} {e}")))?;
    // Defence in depth: the Host's own reading of the path must agree that every component is a plain folder name.
    if !Path::new(&rel)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(Error::Refused(format!(
            "{rel}: {what} must be relative to its root"
        )));
    }
    let canonical_root = root.path.canonicalize().map_err(|e| {
        Error::Refused(format!("root {} ({}): {e}", root.name, root.path.display()))
    })?;
    Ok(Beneath {
        root,
        rel,
        canonical_root,
    })
}

impl Beneath<'_> {
    /// The Root's path as configured, joined with the path.
    fn configured(&self) -> PathBuf {
        self.rel
            .split('/')
            .fold(self.root.path.clone(), |p, part| p.join(part))
    }

    /// The path as configured ([`Beneath::configured`]), checked to be a directory that, after following symlinks,
    /// resolves inside the Root.
    pub fn existing_dir(&self) -> Result<PathBuf> {
        let configured = self.configured();
        let canonical = configured
            .canonicalize()
            .map_err(|e| Error::Refused(format!("{}: {e}", configured.display())))?;
        if !canonical.starts_with(&self.canonical_root) {
            return Err(Error::Refused(format!(
                "{}: resolves outside root {}",
                self.rel, self.root.name
            )));
        }
        if !canonical.is_dir() {
            return Err(Error::Refused(format!(
                "{}: not a directory",
                configured.display()
            )));
        }
        Ok(configured)
    }

    /// Where the folder would be, whether or not it exists yet: [`resolved`] beneath the canonical Root, else the
    /// path joined to it. Not checked to lie inside the Root: a symlink in it may lead out.
    pub fn would_be(&self) -> PathBuf {
        resolved(&self.canonical_root, &self.rel).unwrap_or_else(|| {
            self.rel
                .split('/')
                .fold(self.canonical_root.clone(), |p, part| p.join(part))
        })
    }
}

/// `rel` under `base`, through whatever of it exists: its longest existing ancestor resolved, with the rest, each a
/// plain name, joined on. `None` when a part is not a plain name, or an existing part can't be resolved.
pub(crate) fn resolved(base: &Path, rel: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = rel.split('/').filter(|part| !part.is_empty()).collect();
    if parts.iter().any(|part| matches!(*part, "." | "..")) {
        return None;
    }
    for exists in (0..=parts.len()).rev() {
        let path = parts[..exists]
            .iter()
            .fold(base.to_path_buf(), |p, part| p.join(part));
        match path.canonicalize() {
            Ok(found) => {
                return Some(parts[exists..].iter().fold(found, |p, part| p.join(part)));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    None
}
