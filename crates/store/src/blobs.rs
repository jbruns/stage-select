//! Content-addressed DAT blobs under `dats/<sha256>` (ADR 0003), and the cue sheets of CDs placed as CHD under
//! `cues/<sha256>` (ADR 0023).

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::Path;

use super::{CUE_PACKS, CUES, DATS, Error, Result, SBI_PACKS, SUPPORT_FILES, Store, io_error};

/// The cue sheet blobs a row holds: those of Placed Games' CHDs, and those a placing Build's items recorded once
/// written, until their Games are placed or the placement is undone.
pub(super) const HELD_CUES: &str =
    "SELECT cue_sha256 FROM placed_files WHERE cue_sha256 IS NOT NULL
     UNION SELECT cue_sha256 FROM build_items
     WHERE cue_sha256 IS NOT NULL AND place_state IN ('converting', 'disposing')";

/// Writes a blob to a temp file in the blob directory, fsyncs it, renames it into place and fsyncs the
/// directory, so it is durable before any row refers to it.
pub(super) fn write_blob(dats: &Path, sha256: &str, bytes: &[u8]) -> Result<()> {
    let tmp = dats.join(format!(".{sha256}.tmp"));
    let mut file = File::create(&tmp).map_err(|e| io_error(&tmp, e))?;
    file.write_all(bytes).map_err(|e| io_error(&tmp, e))?;
    file.sync_all().map_err(|e| io_error(&tmp, e))?;
    drop(file);
    let path = dats.join(sha256);
    std::fs::rename(&tmp, &path).map_err(|e| io_error(&path, e))?;
    sync_dir(dats)
}

#[cfg(unix)]
pub(super) fn sync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io_error(dir, e))
}

#[cfg(not(unix))]
pub(super) fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Why a kept cue sheet can't be given back: it was read, and holds other bytes than it was kept as; or it couldn't be
/// read, which says nothing of what it holds.
pub(crate) enum Unkept {
    Changed(String),
    Unread(String),
}

pub(super) fn remove_blob(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_error(path, e)),
        _ => Ok(()),
    }
}

impl Store {
    /// Keeps the cue sheet `bytes` in the store as the blob named by its SHA-256, written, synced and renamed into
    /// place, its folder synced, before any row refers to it. Returns its SHA-256.
    pub(crate) fn keep_cue(&self, bytes: &[u8]) -> Result<String> {
        let sha256 = title_select_engine::sha256_hex(bytes);
        let dir = self.dir.join(CUES);
        std::fs::create_dir_all(&dir).map_err(|e| io_error(&dir, e))?;
        write_blob(&dir, &sha256, bytes)?;
        sync_dir(&self.dir)?;
        Ok(sha256)
    }

    /// The cue sheet kept as the blob `sha256`, read whole and checked to be what its name says.
    pub(crate) fn kept_cue(&self, sha256: &str) -> Result<Vec<u8>> {
        self.kept_cue_as(sha256).map_err(|unkept| match unkept {
            Unkept::Changed(why) | Unkept::Unread(why) => Error::Io(why),
        })
    }

    /// As [`Store::kept_cue`], saying whether a cue sheet it can't give back was read and holds other bytes, or
    /// couldn't be read at all.
    pub(crate) fn kept_cue_as(&self, sha256: &str) -> std::result::Result<Vec<u8>, Unkept> {
        let path = self.cue_path(sha256);
        let file = File::open(&path).map_err(|e| Unkept::Unread(io_error(&path, e).to_string()))?;
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(
            &mut std::io::Read::take(&file, title_select_engine::chd::CUE_MAX + 1),
            &mut bytes,
        )
        .map_err(|e| Unkept::Unread(io_error(&path, e).to_string()))?;
        if bytes.len() as u64 > title_select_engine::chd::CUE_MAX
            || title_select_engine::sha256_hex(&bytes) != sha256
        {
            return Err(Unkept::Changed(format!(
                "the cue sheet kept in the store as {sha256} no longer holds what it was kept as"
            )));
        }
        Ok(bytes)
    }

    /// Deletes every file in the blob directory that is not the blob of a DAT Revision whose row holds it,
    /// including temp files left by an interrupted write, and every file in the cue pack directory that is not the
    /// file of a cue pack whose row holds it, including one whose deletion was recorded but not finished, and SBI packs
    /// and support files alike. Returns the names it deleted, a cue pack's under `cue_packs/`, an SBI pack's under
    /// `sbi_packs/` and a support file's under `support_files/`.
    ///
    /// It holds the writer throughout, so no import in this process is between writing a blob and inserting its
    /// row; the store's lock excludes every other process.
    pub fn collect_garbage(&self) -> Result<Vec<String>> {
        self.with_writer(|conn| {
            let held = |sql: &str| -> Result<HashSet<String>> {
                let mut stmt = conn.prepare(sql)?;
                Ok(stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<HashSet<String>, _>>()?)
            };
            let mut removed = Vec::new();
            for (dir, sql, prefix) in [
                (
                    DATS,
                    "SELECT sha256 FROM dat_revisions WHERE blob_dropped_at IS NULL",
                    "",
                ),
                (
                    CUE_PACKS,
                    "SELECT sha256 FROM cue_packs WHERE blob_dropped_at IS NULL",
                    "cue_packs/",
                ),
                (
                    SBI_PACKS,
                    "SELECT sha256 FROM sbi_packs WHERE blob_dropped_at IS NULL",
                    "sbi_packs/",
                ),
                (
                    SUPPORT_FILES,
                    "SELECT sha256 FROM support_files",
                    "support_files/",
                ),
                (CUES, HELD_CUES, "cues/"),
            ] {
                let dir = self.dir.join(dir);
                // The cue pack directory is made by the first import.
                let items = match std::fs::read_dir(&dir) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound && !prefix.is_empty() => {
                        continue;
                    }
                    items => items.map_err(|e| io_error(&dir, e))?,
                };
                let held = held(sql)?;
                let mut gone = Vec::new();
                for item in items {
                    let item = item.map_err(|e| io_error(&dir, e))?;
                    let name = item.file_name().to_string_lossy().into_owned();
                    let is_file = item.file_type().is_ok_and(|t| t.is_file());
                    if is_file && !held.contains(&name) {
                        remove_blob(&item.path())?;
                        gone.push(format!("{prefix}{name}"));
                    }
                }
                gone.sort();
                removed.extend(gone);
            }
            Ok(removed)
        })
    }
}
