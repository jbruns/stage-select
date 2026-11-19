//! Returning Placed Games (ADR 0023): a Build item of kind `return` takes one release placed in the Library Folder
//! back to its Source Set's folder, whole: every placed file of each of its Games, or none.
//!
//! - **Rename**: each placed file is the file the Source Set stores, on the same filesystem. Each is held open,
//!   checked to be the file the plan found by its device, inode, size, modification time and change time, and renamed
//!   home by a rename that never replaces anything. Each is checked again through the descriptor held across the
//!   rename, below the folders recorded in the Source Set's folder, then the release is recorded home.
//! - **Copy** and **Convert**: each file the Source Set stores is staged in its folder's private area, written from the
//!   placed files, each read whole while it holds still first, and verified against the DAT; then every one is put in
//!   place by a rename that never replaces anything. The placed files that go are moved into the Library Folder's
//!   private `.deleting` folder through a descriptor held across that move, and then, last, every file put home is
//!   checked through the descriptor held on it to still read as it was verified, below the folders recorded, walked
//!   again from the Source Set's registered path with its marker, and each placed file set aside to still be the file
//!   read. Only then is the release recorded home, in one transaction, and only then is each placed file deleted. If
//!   any check fails, each placed file is put back and every file put home removed: the release stays placed. A Game
//!   still selected in the folder keeps its file there, as a built copy, and only stops being placed.
//!
//! Each step is recorded before it is taken. Until the release is recorded home, recovery rolls every step back, and
//! once it is, forward; one it can't finish is left outstanding, with why, for the next start. So at every step each
//! Game has a recorded copy: its placed files, set aside to delete at most, until it is recorded home; then the files
//! put home, verified.

use std::fs::File;
use std::io;
use std::time::SystemTime;

use rusqlite::{OptionalExtension, Transaction, params};
use rustix::fs::{FileType, Stat};
use title_select_api::build::{
    ItemKind, ItemState, Marker, PlacedOriginal, PlanItem, PlanReturn, PlanSource, ReturnFile,
    ReturnHow, ReturnPlaced,
};
use title_select_api::lifecycle::RegistrationKind;
use title_select_api::storage::placed_path;
use title_select_engine::build::{verify, verify_loose};
use title_select_engine::meter::Tally;

use super::place::SetDirs;
use super::{Failure, remove_layout_dir, set_item, stage_by_fd, still_sha256};
use crate::carry::Held;
use crate::config::Config;
use crate::folder_dir::{
    FolderDir, Identity, create_staged, ctime_ns, file_stat, parent_made, parent_of,
    remove_emptied, staged_name,
};
use crate::folders::{check_marker, open_writable_folder};
use crate::ingest::moves::descend;
use crate::kept::{Scope, open_area_deleting, unique};
use crate::repairs::{MarkerRule, open_repairable};
use crate::{Error, Fault, Result, Store};

fn io_err(e: impl std::fmt::Display) -> Error {
    Error::Io(e.to_string())
}

/// What returning a release needs of its Build.
pub(super) struct Returning<'a> {
    pub build: i64,
    pub folder: i64,
    pub set: i64,
    pub set_name: &'a str,
    /// The Library Folder, opened to write to.
    pub dir: &'a FolderDir,
    pub config: &'a Config,
    /// Where what is written and read back in the Source Set's Root is counted, and what is read in the Library
    /// Folder's.
    pub to: &'a Tally,
    pub from: &'a Tally,
}

/// Where a file a return puts home goes in the Source Set's folder: the folder it goes in, opened from the Source Set's
/// folder without following a symbolic link, its name there, and the identity of each folder from the Source Set's
/// folder down to it.
struct Home {
    dir: FolderDir,
    leaf: String,
    chain: Vec<Identity>,
}

fn leaf(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// Makes each folder `path` goes in, in the Source Set's folder `set`, without following a symbolic link, and opens
/// the one it goes in.
fn home(set: &FolderDir, path: &str) -> io::Result<Home> {
    parent_made(set, path)?;
    let (dir, chain) = descend(set.try_clone()?, path)?;
    Ok(Home {
        dir,
        leaf: leaf(path),
        chain,
    })
}

/// Where `path` went, in the Source Set's folder `set` opened now, while every folder down to it is `chain`, the ones
/// recorded before anything of its release was put home. Returns why not.
fn recorded_home(
    set: &FolderDir,
    path: &str,
    chain: Option<&[Identity]>,
) -> io::Result<std::result::Result<Home, String>> {
    let Some(chain) = chain else {
        return Ok(Err(format!("where {path} was put isn't recorded")));
    };
    Ok(match descend(set.try_clone()?, path) {
        Ok((dir, now)) if now == chain => Ok(Home {
            dir,
            leaf: leaf(path),
            chain: now,
        }),
        Ok(_) => Err(format!(
            "the folder {path} was put in is no longer the one in the source set's folder"
        )),
        Err(e) => Err(format!(
            "the folder {path} was put in can't be opened ({e})"
        )),
    })
}

fn is_regular(st: &Stat, id: Identity) -> bool {
    FolderDir::kind(st) == FileType::RegularFile && Identity::of(st) == id
}

/// The placed file `pl` in the Library Folder `dir`, held open while it is the regular file the plan found: the same
/// device and inode, size, modification time and change time. Returns why not.
fn hold(dir: &FolderDir, pl: &ReturnPlaced) -> Result<std::result::Result<File, String>> {
    let (parent, name) = match parent_of(dir, &pl.path) {
        Ok(found) => found,
        Err(e) => return Ok(Err(format!("{} can't be reached: {e}", pl.path))),
    };
    let Some(file) = parent
        .as_ref()
        .unwrap_or(dir)
        .pin(name, Identity::from(pl.identity))
    else {
        return Ok(Err(format!(
            "{} is no longer the file the plan found",
            pl.path
        )));
    };
    let st = rustix::fs::fstat(&file).map_err(io_err)?;
    if file_stat(&st) != pl.file || ctime_ns(&st) != pl.ctime_ns {
        return Ok(Err(format!("{} changed since the plan was made", pl.path)));
    }
    Ok(Ok(file))
}

/// Whether the placed file `pl` is at its name in the Library Folder `dir`.
fn still_placed(dir: &FolderDir, pl: &ReturnPlaced) -> Result<bool> {
    Ok(dir
        .stat(&pl.path)
        .map_err(io_err)?
        .is_some_and(|st| is_regular(&st, Identity::from(pl.identity))))
}

fn original(pl: &ReturnPlaced) -> PlacedOriginal {
    PlacedOriginal {
        path: pl.path.clone(),
        file: pl.file.clone(),
        identity: pl.identity,
        ctime_ns: pl.ctime_ns,
    }
}

/// The file `f` a return writes home, as an item to stage: a copy of its placed file, whole, or a repack from its
/// ROMs.
fn output_item(f: &ReturnFile) -> PlanItem {
    PlanItem {
        group: String::new(),
        game: f.game.clone(),
        target: f.path.clone(),
        kind: match f.from {
            Some(_) => ItemKind::Copy,
            None => ItemKind::Repack,
        },
        loose: f.loose,
        chd: false,
        roms: f.roms.clone(),
        sources: f.sources.clone(),
        reason: None,
        existing: None,
        adoptable: false,
        keepable: false,
        keep: None,
        converts: None,
        addition: None,
        generated: None,
        place: None,
        changes_source_set: true,
    }
}

/// Opens `source`, a placed file by its path in the Library Folder `dir`, without following a symbolic link, while it
/// is the regular file the plan found there.
fn open_placed(dir: &FolderDir, source: &PlanSource) -> std::result::Result<File, String> {
    let file = dir
        .open_file(&source.path)
        .map_err(|e| format!("{}: {e}", source.path))?;
    let st = rustix::fs::fstat(&file).map_err(|e| format!("{}: {e}", source.path))?;
    if source.identity.map(Identity::from) != Some(Identity::of(&st))
        || source.ctime_ns != Some(ctime_ns(&st))
        || file_stat(&st) != source.file
    {
        return Err(format!(
            "{} is not the file the plan found there; plan again",
            source.path
        ));
    }
    Ok(file)
}

/// A placed file a return item recorded: where it was set aside to delete, what it read as, and what became of it.
struct PlacedRow {
    n: usize,
    sha256: Option<String>,
    slot: Option<String>,
    moved_ctime: Option<i128>,
    state: String,
}

/// A file a return item puts home, as it recorded it.
struct OutputRow {
    staged: Option<String>,
    id: Option<Identity>,
    sha256: Option<String>,
    chain: Option<Vec<Identity>>,
}

fn placed_rows(c: &rusqlite::Connection, build: i64, seq: i64) -> Result<Vec<PlacedRow>> {
    let mut stmt = c.prepare(
        "SELECT n, sha256, slot, moved_ctime_ns, state FROM return_placed
         WHERE build_id = ?1 AND seq = ?2 ORDER BY n",
    )?;
    let rows = stmt.query_map(params![build, seq], |r| {
        Ok(PlacedRow {
            n: r.get::<_, i64>(0)? as usize,
            sha256: r.get(1)?,
            slot: r.get(2)?,
            moved_ctime: r
                .get::<_, Option<String>>(3)?
                .and_then(|ns| ns.parse().ok()),
            state: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn output_rows(c: &rusqlite::Connection, build: i64, seq: i64) -> Result<Vec<OutputRow>> {
    let mut stmt = c.prepare(
        "SELECT staged, dev, ino, sha256, chain FROM return_outputs
         WHERE build_id = ?1 AND seq = ?2 ORDER BY n",
    )?;
    let rows = stmt.query_map(params![build, seq], |r| {
        let id = match (r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?) {
            (Some(dev), Some(ino)) => Some(Identity::from_sql(dev, ino)),
            _ => None,
        };
        let chain = r
            .get::<_, Option<String>>(4)?
            .and_then(|json| serde_json::from_str::<Vec<(i64, i64)>>(&json).ok())
            .map(|chain| {
                chain
                    .into_iter()
                    .map(|(dev, ino)| Identity::from_sql(dev, ino))
                    .collect()
            });
        Ok(OutputRow {
            staged: r.get(0)?,
            id,
            sha256: r.get(3)?,
            chain,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn chain_json(chain: &[Identity]) -> Result<String> {
    let chain: Vec<(i64, i64)> = chain.iter().map(|id| id.to_sql()).collect();
    serde_json::to_string(&chain).map_err(io_err)
}

fn set_return_state(tx: &Transaction, build: i64, seq: i64, state: Option<&str>) -> Result<()> {
    tx.execute(
        "UPDATE build_items SET return_state = ?3 WHERE build_id = ?1 AND seq = ?2",
        params![build, seq, state],
    )?;
    Ok(())
}

fn set_placed(
    tx: &Transaction,
    build: i64,
    (seq, n): (i64, usize),
    state: &str,
    detail: Option<&str>,
) -> Result<()> {
    tx.execute(
        "UPDATE return_placed SET state = ?4, detail = ?5 WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
        params![build, seq, n as i64, state, detail],
    )?;
    Ok(())
}

impl Store {
    /// Returns the release of `r`, Build item `seq`, to its Source Set's folder, as its plan says: every file of it,
    /// or none. A failure that leaves it placed, as it was, fails only the item; one that leaves anything to finish
    /// stops the Build, for recovery.
    pub(super) fn return_release(
        &self,
        p: &Returning,
        set: &SetDirs,
        seq: i64,
        r: &PlanReturn,
        built_at: SystemTime,
    ) -> Result<()> {
        self.write(|tx| {
            for (n, pl) in r.placed.iter().enumerate() {
                tx.execute(
                    "INSERT OR REPLACE INTO return_placed
                         (build_id, seq, n, path, game, size, mtime_ns, dev, ino, ctime_ns, keeps, state)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'placed')",
                    params![
                        p.build,
                        seq,
                        n as i64,
                        pl.path,
                        pl.game,
                        pl.file.size as i64,
                        pl.file.mtime_ns as i64,
                        pl.identity.dev as i64,
                        pl.identity.ino as i64,
                        pl.ctime_ns as i64,
                        pl.keeps
                    ],
                )?;
            }
            for (n, f) in r.files.iter().enumerate() {
                tx.execute(
                    "INSERT OR REPLACE INTO return_outputs (build_id, seq, n, path) VALUES (?1, ?2, ?3, ?4)",
                    params![p.build, seq, n as i64, f.path],
                )?;
            }
            Ok(())
        })?;
        let outcome = match r.how {
            ReturnHow::Rename => self.return_rename(p, set, seq, r, built_at)?,
            ReturnHow::Copy | ReturnHow::Convert => self.return_write(p, set, seq, r, built_at)?,
        };
        outcome.map_err(Error::Refused)
    }

    /// Fails item `seq` with `why`, its release left placed, as it was.
    fn fail_return(
        &self,
        p: &Returning,
        seq: i64,
        r: &PlanReturn,
        why: &str,
    ) -> Result<std::result::Result<(), String>> {
        self.write(|tx| {
            set_return_state(tx, p.build, seq, None)?;
            set_item(
                tx,
                p.build,
                seq,
                ItemState::Failed,
                &format!("{why}; {} stays placed in this folder", r.release),
            )
        })?;
        Ok(Ok(()))
    }

    /// For each file of a rename, the placed file it is: by index into the plan's files and placed files.
    fn pairs(r: &PlanReturn) -> Option<Vec<(usize, usize)>> {
        r.files
            .iter()
            .enumerate()
            .map(|(n, f)| {
                let from = f.from.as_deref()?;
                Some((n, r.placed.iter().position(|pl| pl.path == from)?))
            })
            .collect()
    }

    fn return_rename(
        &self,
        p: &Returning,
        set: &SetDirs,
        seq: i64,
        r: &PlanReturn,
        built_at: SystemTime,
    ) -> Result<std::result::Result<(), String>> {
        let Some(pairs) = Self::pairs(r).filter(|pairs| pairs.len() == r.placed.len()) else {
            return self.fail_return(
                p,
                seq,
                r,
                "its files can't each be renamed home from one placed file; plan again",
            );
        };
        let mut held = Vec::new();
        for &(_, k) in &pairs {
            match hold(p.dir, &r.placed[k])? {
                Ok(file) => held.push(file),
                Err(why) => return self.fail_return(p, seq, r, &why),
            }
        }
        let mut homes = Vec::new();
        for &(n, _) in &pairs {
            match home(&set.dir, &r.files[n].path) {
                Ok(h) => homes.push(h),
                Err(e) => {
                    let why = format!(
                        "the folder {} goes in, in source set {}, could not be made: {e}",
                        r.files[n].path, p.set_name
                    );
                    return self.fail_return(p, seq, r, &why);
                }
            }
        }
        self.write(|tx| {
            for (&(n, _), h) in pairs.iter().zip(&homes) {
                tx.execute(
                    "UPDATE return_outputs SET chain = ?4 WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                    params![p.build, seq, n as i64, chain_json(&h.chain)?],
                )?;
            }
            set_return_state(tx, p.build, seq, Some("renaming"))
        })?;
        let mut moved: Vec<usize> = Vec::new();
        let mut moved_ctimes: Vec<i128> = Vec::new();
        for (i, (&(_, k), h)) in pairs.iter().zip(&homes).enumerate() {
            let pl = &r.placed[k];
            self.fault_point(Fault::ReturnRenaming(seq));
            let (parent, name) = match parent_of(p.dir, &pl.path) {
                Ok(found) => found,
                Err(e) => {
                    let why = format!("{} could not be moved home: {e}", pl.path);
                    return self.unrename(p, seq, r, &pairs, &homes, &moved, &why);
                }
            };
            // Last, before the rename: through the descriptor held, still the file the plan found, its whole stamp,
            // change time too, as it was, so that a change made before the rename is never taken home. Then the
            // rename, and its change time read through that descriptor as the very next thing: a change made between
            // the two, keeping its size and modification time, is the one window left open, as a rename reads no
            // file, like a change made after the last check.
            let before = rustix::fs::fstat(&held[i]).map_err(io_err)?;
            if Identity::of(&before) != Identity::from(pl.identity)
                || file_stat(&before) != pl.file
                || ctime_ns(&before) != pl.ctime_ns
            {
                let why = format!("{} changed since the plan was made", pl.path);
                return self.unrename(p, seq, r, &pairs, &homes, &moved, &why);
            }
            let renamed = parent
                .as_ref()
                .unwrap_or(p.dir)
                .rename_new(name, &h.dir, &h.leaf);
            let after = renamed.as_ref().ok().map(|()| rustix::fs::fstat(&held[i]));
            if let Err(e) = renamed {
                if moved.is_empty() && self.across_filesystems(Some(&e)) {
                    // No rename reaches the Source Set's folder from here: the release is copied home instead.
                    drop(held);
                    self.write(|tx| set_return_state(tx, p.build, seq, None))?;
                    return self.return_write(p, set, seq, r, built_at);
                }
                let why = format!("{} could not be moved home: {e}", pl.path);
                return self.unrename(p, seq, r, &pairs, &homes, &moved, &why);
            }
            moved.push(i);
            // Its change time, read right after the rename, stands for it from here: the last check, and recovery,
            // take it as home only while it still has it.
            let ctime = ctime_ns(&after.expect("renamed").map_err(io_err)?);
            self.write(|tx| {
                tx.execute(
                    "UPDATE return_placed SET moved_ctime_ns = ?4 WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                    params![p.build, seq, k as i64, ctime.to_string()],
                )?;
                Ok(())
            })?;
            moved_ctimes.push(ctime);
            self.fault_point(Fault::ReturnMoved(seq));
        }
        // Every file moved: each checked, through the descriptor held across its rename, to be the file the plan
        // found, unchanged since it was moved, and to be in place, below the folders recorded; then all recorded
        // home together, or all put back.
        let mut found = Vec::new();
        for (((&(n, k), h), file), &moved_ctime) in
            pairs.iter().zip(&homes).zip(&held).zip(&moved_ctimes)
        {
            let pl = &r.placed[k];
            if let Ok((parent, _)) = parent_of(p.dir, &pl.path) {
                parent.as_ref().unwrap_or(p.dir).sync().map_err(io_err)?;
            }
            h.dir.sync().map_err(io_err)?;
            let id = Identity::from(pl.identity);
            let at = h.dir.stat(&h.leaf).map_err(io_err)?;
            let now = rustix::fs::fstat(file).map_err(io_err)?;
            let why = match &at {
                Some(at)
                    if is_regular(at, id)
                        && Identity::of(&now) == id
                        && file_stat(&now) == pl.file
                        && ctime_ns(&now) == moved_ctime =>
                {
                    self.in_set(p, &h.chain, &r.files[n].path)?.err()
                }
                Some(at) if is_regular(at, id) => {
                    Some(format!("{} changed as it was moved", pl.path))
                }
                _ => Some(format!("{} was replaced as it was moved", pl.path)),
            };
            if let Some(why) = why {
                let all: Vec<usize> = (0..pairs.len()).collect();
                return self.unrename(p, seq, r, &pairs, &homes, &all, &why);
            }
            found.push(now);
        }
        self.commit_rename(p, seq, r, &pairs, &found)?;
        Ok(Ok(()))
    }

    /// Records the release of `r` home, its placed files renamed there and found as `found`: each placed file
    /// forgotten, and its Source Set's latest Audit made to name it home, with its Evidence. In one transaction.
    fn commit_rename(
        &self,
        p: &Returning,
        seq: i64,
        r: &PlanReturn,
        pairs: &[(usize, usize)],
        found: &[Stat],
    ) -> Result<()> {
        self.write(|tx| {
            let audit = latest_audit(tx, p.set)?;
            for (&(n, k), now) in pairs.iter().zip(found) {
                let pl = &r.placed[k];
                crate::placements::forget_placed(tx, p.folder, &pl.path)?;
                tx.execute(
                    "DELETE FROM built_files WHERE folder_id = ?1 AND target = ?2",
                    params![p.folder, pl.path],
                )?;
                if let Some(audit) = audit {
                    crate::placements::moved_entries(
                        tx,
                        (p.set, audit),
                        &placed_path(p.folder, &pl.path),
                        &r.files[n].path,
                        &original(pl),
                        now,
                    )?;
                }
                set_placed(tx, p.build, (seq, k), "deleted", None)?;
            }
            set_return_state(tx, p.build, seq, Some("done"))?;
            tx.execute(
                "UPDATE build_items SET state = 'applied', error = NULL WHERE build_id = ?1 AND seq = ?2",
                params![p.build, seq],
            )?;
            Ok(())
        })?;
        self.fault_point(Fault::ReturnRecorded(seq));
        for pl in &r.placed {
            remove_layout_dir(self, p.dir, p.folder, &pl.path)?;
        }
        Ok(())
    }

    /// Puts each placed file of `moved`, indexes into `pairs` renamed home by this Build, back where it was, then fails
    /// item `seq` with `why`. Whatever was renamed goes back, whether it is the file the plan found or one that took
    /// its name first. `Err` says why one that can't be put back leaves the item outstanding.
    #[allow(clippy::too_many_arguments)]
    fn unrename(
        &self,
        p: &Returning,
        seq: i64,
        r: &PlanReturn,
        pairs: &[(usize, usize)],
        homes: &[Home],
        moved: &[usize],
        why: &str,
    ) -> Result<std::result::Result<(), String>> {
        for &i in moved {
            let (n, k) = pairs[i];
            let pl = &r.placed[k];
            let h = &homes[i];
            if let Err(e) = self.unrename_one(p, (seq, k), pl, h) {
                return Ok(Err(format!(
                    "{why}, and {} could not be put back in the play library folder ({e}); it is at {} until it \
                     can be",
                    pl.path,
                    h.dir.path().join(&h.leaf).display()
                )));
            }
            self.reverify(p, (seq, k), pl, &r.files[n])?;
        }
        let why = match moved.is_empty() {
            true => why.to_owned(),
            false => format!("{why}, so its files were put back"),
        };
        self.fail_return(p, seq, r, &why)
    }

    /// Records the placed file `pl`, put back where it was placed after a rename home gave it a new change time, as
    /// found: while it is the file the plan found, of its size and modification time, verified against the DAT as
    /// `f`, the file it was to be home, then read whole through its own descriptor while that holds. Placed by a
    /// rename, it was never read, and its change time alone stood for it. Otherwise it is left recorded as it was,
    /// for a check to flag as changed.
    fn reverify(
        &self,
        p: &Returning,
        (seq, k): (i64, usize),
        pl: &ReturnPlaced,
        f: &ReturnFile,
    ) -> Result<()> {
        let found = self.verified_back(p, pl, f)?;
        // Settled either way, its mark comes off with what is recorded of it: a stop before leaves it to recovery.
        self.write(|tx| {
            if let Some((st, sha)) = &found {
                let id = Identity::from(pl.identity);
                crate::placements::reread(tx, p.folder, &pl.path, (id, &pl.file), st, sha)?;
            }
            set_placed(tx, p.build, (seq, k), "placed", None)
        })
    }

    /// The placed file `pl`, back where it was placed, as statted and read, when it is the file the plan found, of
    /// its size and modification time, verified against the DAT as `f`, then read whole through its own descriptor
    /// while that holds; or None.
    fn verified_back(
        &self,
        p: &Returning,
        pl: &ReturnPlaced,
        f: &ReturnFile,
    ) -> Result<Option<(Stat, String)>> {
        let Ok(file) = p.dir.open_file(&pl.path) else {
            return Ok(None);
        };
        let st = rustix::fs::fstat(&file).map_err(io_err)?;
        if !is_regular(&st, Identity::from(pl.identity)) || file_stat(&st) != pl.file {
            return Ok(None);
        }
        let verified = match (f.loose, f.roms.as_slice()) {
            (true, [rom]) => verify_loose(&file, rom, p.from),
            (true, _) => Err("a loose file holds one ROM".into()),
            (false, roms) => verify(&file, roms, p.from),
        };
        if verified.is_err() {
            return Ok(None);
        }
        Ok(still_sha256(&file, ctime_ns(&st), p.from)
            .map_err(io_err)?
            .map(|sha| (st, sha)))
    }

    /// Puts the placed file `pl`, placed file `k` of item `seq`, renamed home to `h`, back where it was placed, marked
    /// first as being put back: until [`Store::reverify`] settles it, recovery verifies it again wherever it finds it
    /// placed.
    fn unrename_one(
        &self,
        p: &Returning,
        (seq, k): (i64, usize),
        pl: &ReturnPlaced,
        h: &Home,
    ) -> std::result::Result<(), Error> {
        self.write(|tx| set_placed(tx, p.build, (seq, k), "putting_back", None))?;
        move_back(p.dir, pl, &h.dir, &h.leaf).map_err(io_err)?;
        self.fault_point(Fault::ReturnPutBack(seq));
        Ok(())
    }

    /// Whether `path` is still below `chain`, the folders it was put in, in the Source Set's folder at its registered
    /// path, with its marker: each walked again from there without following a symbolic link. Returns why not.
    fn in_set(
        &self,
        p: &Returning,
        chain: &[Identity],
        path: &str,
    ) -> Result<std::result::Result<(), String>> {
        let dir = match open_repairable(self, p.config, p.set, MarkerRule::Required)? {
            Ok(dir) => dir,
            Err(why) => {
                return Ok(Err(format!(
                    "source set {}'s folder can't be opened where it is registered: {why}",
                    p.set_name
                )));
            }
        };
        Ok(match descend(dir, path) {
            Ok((_, now)) if now == chain => Ok(()),
            Ok(_) => Err(format!(
                "the folder {path} was put in is no longer the one at source set {}'s path",
                p.set_name
            )),
            Err(e) => Err(format!(
                "the folder {path} was put in can't be opened from source set {}'s path ({e})",
                p.set_name
            )),
        })
    }

    fn return_write(
        &self,
        p: &Returning,
        set: &SetDirs,
        seq: i64,
        r: &PlanReturn,
        built_at: SystemTime,
    ) -> Result<std::result::Result<(), String>> {
        // Each placed file that goes is read whole while it holds still, and recorded, before anything is written.
        let mut read = Vec::new();
        for (k, pl) in r.placed.iter().enumerate() {
            let held = match hold(p.dir, pl)? {
                Ok(held) => held,
                Err(why) => return self.fail_return(p, seq, r, &why),
            };
            if pl.keeps {
                continue;
            }
            match still_sha256(&held, pl.ctime_ns, p.from).map_err(io_err)? {
                Some(sha) => read.push((k, sha)),
                None => {
                    let why = format!("{} changed while it was read", pl.path);
                    return self.fail_return(p, seq, r, &why);
                }
            }
        }
        self.write(|tx| {
            for (k, sha) in &read {
                tx.execute(
                    "UPDATE return_placed SET sha256 = ?4 WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                    params![p.build, seq, *k as i64, sha],
                )?;
            }
            set_return_state(tx, p.build, seq, Some("writing"))
        })?;
        // Every file written home and verified against the DAT in the Source Set's private folder before any is put
        // in place.
        let mut ready = Vec::new();
        for (n, f) in r.files.iter().enumerate() {
            let staged = staged_name(&leaf(&f.path));
            self.write(|tx| {
                tx.execute(
                    "UPDATE return_outputs SET staged = ?4 WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                    params![p.build, seq, n as i64, staged],
                )?;
                Ok(())
            })?;
            let mut file = match create_staged(&set.dir, &staged) {
                Ok(file) => file,
                Err(e) => {
                    let why = format!("{} could not be written: {e}", f.path);
                    return self.unwind(p, set, seq, r, &why);
                }
            };
            let id = Identity::of_fd(&file).map_err(io_err)?;
            let (dev, ino) = id.to_sql();
            self.write(|tx| {
                tx.execute(
                    "UPDATE return_outputs SET dev = ?4, ino = ?5 WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                    params![p.build, seq, n as i64, dev, ino],
                )?;
                Ok(())
            })?;
            let item = output_item(f);
            let open = |source: &PlanSource| open_placed(p.dir, source);
            match stage_by_fd(&item, &mut file, &open, (p.from, p.to), built_at) {
                Ok(done) => ready.push((staged, id, done)),
                Err(Failure::Item(why) | Failure::Folder(why)) => {
                    drop(file);
                    return self.unwind(p, set, seq, r, &why);
                }
                Err(Failure::Cancelled) => {
                    drop(file);
                    return self.unwind(p, set, seq, r, "you cancelled the build");
                }
            }
            self.fault_point(Fault::ReturnStaged(seq));
        }
        let mut homes = Vec::new();
        for f in &r.files {
            match home(&set.dir, &f.path) {
                Ok(h) => homes.push(h),
                Err(e) => {
                    let why = format!(
                        "the folder {} goes in, in source set {}, could not be made: {e}",
                        f.path, p.set_name
                    );
                    return self.unwind(p, set, seq, r, &why);
                }
            }
        }
        self.write(|tx| {
            for (n, ((_, _, done), h)) in ready.iter().zip(&homes).enumerate() {
                tx.execute(
                    "UPDATE return_outputs SET sha256 = ?4, size = ?5, chain = ?6
                     WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                    params![
                        p.build,
                        seq,
                        n as i64,
                        done.sha256,
                        done.size as i64,
                        chain_json(&h.chain)?
                    ],
                )?;
            }
            set_return_state(tx, p.build, seq, Some("publishing"))
        })?;
        for ((staged, id, _), (h, f)) in ready.iter().zip(homes.iter().zip(&r.files)) {
            let ours = set
                .dir
                .stat(staged)
                .map_err(io_err)?
                .is_some_and(|st| is_regular(&st, *id));
            let put = match ours {
                true => set
                    .dir
                    .rename_new(staged, &h.dir, &h.leaf)
                    .map_err(|e| format!("{} could not be put in place: {e}", f.path)),
                false => Err(format!(
                    "{} was replaced after it was verified",
                    set.dir.path().join(staged).display()
                )),
            };
            if let Err(why) = put {
                return self.unwind(p, set, seq, r, &why);
            }
            self.fault_point(Fault::ReturnPublished(seq));
            h.dir.sync().map_err(io_err)?;
            set.dir.sync_holding(staged).map_err(io_err)?;
        }
        self.finish_write(p, set, seq, r, &homes)
    }

    /// Takes the release of `r`, its files put home at `homes`, the rest of the way: the placed files that go set
    /// aside, every file put home and every one set aside checked last, then the release recorded home and the placed
    /// files deleted. If a check fails, the release is put back as it was.
    fn finish_write(
        &self,
        p: &Returning,
        set: &SetDirs,
        seq: i64,
        r: &PlanReturn,
        homes: &[Home],
    ) -> Result<std::result::Result<(), String>> {
        let outputs = self.read(|c| output_rows(c, p.build, seq))?;
        // Held from before any placed file is set aside until the last check, so that the file checked is the one held.
        let mut held = Vec::new();
        for ((o, h), f) in outputs.iter().zip(homes).zip(&r.files) {
            let opened = match o.id {
                Some(id) => Held::open(&h.dir, &h.leaf, id)?,
                None => Err("nothing written for it is recorded".to_owned()),
            };
            match opened {
                Ok(file) => held.push(file),
                Err(why) => return self.unwind(p, set, seq, r, &format!("{}: {why}", f.path)),
            }
        }
        let deleting = match open_area_deleting(self, Scope::Library(p.folder), p.dir, true)? {
            Ok(Some(deleting)) => deleting,
            Ok(None) => {
                let why = "the play library folder's private .deleting folder is missing";
                return self.unwind(p, set, seq, r, why);
            }
            Err(why) => return self.unwind(p, set, seq, r, &why),
        };
        self.write(|tx| set_return_state(tx, p.build, seq, Some("aside")))?;
        for (k, pl) in r.placed.iter().enumerate().filter(|(_, pl)| !pl.keeps) {
            let slot = format!("returned-{}", unique());
            let save = |slot: Option<&str>, ctime: Option<i128>| {
                self.write(|tx| {
                    tx.execute(
                        "UPDATE return_placed SET slot = ?4, moved_ctime_ns = ?5
                         WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                        params![p.build, seq, k as i64, slot, ctime.map(|ns| ns.to_string())],
                    )?;
                    Ok(())
                })
            };
            save(Some(&slot), None)?;
            let (parent, name) = match parent_of(p.dir, &pl.path) {
                Ok(found) => found,
                Err(e) => {
                    save(None, None)?;
                    let why = format!("{} can't be reached: {e}", pl.path);
                    return self.unwind(p, set, seq, r, &why);
                }
            };
            let dir = parent.as_ref().unwrap_or(p.dir);
            let Some(pin) = dir.pin(name, Identity::from(pl.identity)) else {
                save(None, None)?;
                let why = format!("{} is no longer the file the plan found", pl.path);
                return self.unwind(p, set, seq, r, &why);
            };
            if let Err(e) = dir.rename_new(name, &deleting, &slot) {
                save(None, None)?;
                let why = format!("{} could not be set aside to delete: {e}", pl.path);
                return self.unwind(p, set, seq, r, &why);
            }
            // Its change time, which the rename moved, read through the descriptor held across it.
            let moved = rustix::fs::fstat(&pin).map_err(io_err)?;
            if Identity::of(&moved) == Identity::from(pl.identity) {
                save(Some(&slot), Some(ctime_ns(&moved)))?;
            }
            self.fault_point(Fault::ReturnSetAside(seq));
            dir.sync().map_err(io_err)?;
        }
        deleting.sync().map_err(io_err)?;
        self.fault_point(Fault::ReturnChecking(seq));
        // Checked last, once nothing but recording it and the deletions is left: every file put home, through the
        // descriptor held since before any placed file was set aside, and each placed file set aside. A change after
        // this check can't be stopped by any lock; it is as any later change to a file of a Source Set, which the next
        // Audit finds.
        let checked = self.last_check(p, seq, r, (&outputs, homes, &held), &deleting)?;
        if let Err(why) = checked {
            return self.unwind(p, set, seq, r, &why);
        }
        self.commit_write(p, seq, r)?;
        self.fault_point(Fault::ReturnRecorded(seq));
        self.dispose_placed(p, seq, r, Some(&deleting), false)
    }

    /// Whether every file of `r` put home still reads, through the descriptor held on it, as it was verified, below the
    /// folders recorded; and every placed file set aside to delete is still the file read. Returns why not.
    fn last_check(
        &self,
        p: &Returning,
        seq: i64,
        r: &PlanReturn,
        (outputs, homes, held): (&[OutputRow], &[Home], &[Held]),
        deleting: &FolderDir,
    ) -> Result<std::result::Result<(), String>> {
        for (((o, h), file), f) in outputs.iter().zip(homes).zip(held).zip(&r.files) {
            let Some(sha) = &o.sha256 else {
                return Ok(Err(format!(
                    "what {} was verified as isn't recorded",
                    f.path
                )));
            };
            if let Err(why) = file.still(&h.dir, &h.leaf, sha, p.to)? {
                return Ok(Err(format!("{}: {why}", f.path)));
            }
            if let Err(why) = self.in_set(p, &h.chain, &f.path)? {
                return Ok(Err(why));
            }
        }
        for row in self.read(|c| placed_rows(c, p.build, seq))? {
            let pl = &r.placed[row.n];
            if pl.keeps {
                continue;
            }
            let (Some(slot), Some(ctime), Some(sha)) = (&row.slot, row.moved_ctime, &row.sha256)
            else {
                return Ok(Err(format!(
                    "{} was not set aside to delete as recorded",
                    pl.path
                )));
            };
            if !unchanged(deleting, slot, pl, ctime, sha, p.from)? {
                return Ok(Err(format!(
                    "{} changed after it was read, so it could not be confirmed unchanged",
                    pl.path
                )));
            }
        }
        Ok(Ok(()))
    }

    /// Records the release of `r` home, every file of it put home and checked: each placed file forgotten, the
    /// built record of each that goes dropped, and its Source Set's latest Audit made to name the files home. In one
    /// transaction, which commits it: from here it is rolled forward.
    fn commit_write(&self, p: &Returning, seq: i64, r: &PlanReturn) -> Result<()> {
        self.write(|tx| {
            if let Some(audit) = latest_audit(tx, p.set)? {
                crate::placements::returned_entries(tx, (p.set, audit), p.folder, r)?;
            } else {
                for pl in &r.placed {
                    tx.execute(
                        "DELETE FROM payload_reads WHERE source_set_id = ?1 AND path = ?2",
                        params![p.set, placed_path(p.folder, &pl.path)],
                    )?;
                }
            }
            for pl in &r.placed {
                crate::placements::forget_placed(tx, p.folder, &pl.path)?;
                if !pl.keeps {
                    tx.execute(
                        "DELETE FROM built_files WHERE folder_id = ?1 AND target = ?2",
                        params![p.folder, pl.path],
                    )?;
                    continue;
                }
                // Kept here, it is no longer the Game's only copy, but an ordinary built one: copied whole, when the
                // Source Set's file is written from it whole, or else written from the verified ROMs.
                let whole = r.files.iter().any(|f| f.from.as_deref() == Some(&pl.path));
                tx.execute(
                    "UPDATE built_files SET kind = ?3 WHERE folder_id = ?1 AND target = ?2 AND kind = 'place'",
                    params![
                        p.folder,
                        pl.path,
                        match whole {
                            true => ItemKind::Copy.as_str(),
                            false => ItemKind::Repack.as_str(),
                        }
                    ],
                )?;
            }
            set_return_state(tx, p.build, seq, Some("committed"))?;
            tx.execute(
                "UPDATE build_items SET state = 'applied', error = NULL WHERE build_id = ?1 AND seq = ?2",
                params![p.build, seq],
            )?;
            Ok(())
        })
    }

    /// Deletes each placed file of `r`, its release recorded home, from the Library Folder's private `.deleting`
    /// folder `deleting`. Found there after a stop, `recovering`, it is deleted only while it is still the file read
    /// and checked; otherwise it is put back where it was, never deleted unproven, and the item says so.
    fn dispose_placed(
        &self,
        p: &Returning,
        seq: i64,
        r: &PlanReturn,
        deleting: Option<&FolderDir>,
        recovering: bool,
    ) -> Result<std::result::Result<(), String>> {
        let mut left = Vec::new();
        for row in self.read(|c| placed_rows(c, p.build, seq))? {
            let pl = &r.placed[row.n];
            if pl.keeps || row.state != "placed" {
                continue;
            }
            let (Some(slot), Some(deleting)) = (&row.slot, deleting) else {
                return Ok(Err(format!(
                    "{} is recorded home, but where {} was set aside to delete isn't recorded; it is left as it \
                     is",
                    r.release, pl.path
                )));
            };
            let id = Identity::from(pl.identity);
            let found = deleting.stat(slot).map_err(io_err)?;
            let state = match found {
                // Deleted before a stop.
                None => ("deleted", None),
                Some(st) if is_regular(&st, id) => {
                    let proven = !recovering
                        || match (row.moved_ctime, &row.sha256) {
                            (Some(ctime), Some(sha)) => {
                                unchanged(deleting, slot, pl, ctime, sha, p.from)?
                            }
                            _ => false,
                        };
                    match proven {
                        true => {
                            deleting.unlink(slot).map_err(io_err)?;
                            deleting.sync().map_err(io_err)?;
                            ("deleted", None)
                        }
                        false => {
                            let why = match move_back_from(p.dir, pl, deleting, slot) {
                                Ok(()) => format!(
                                    "{} changed after it was checked, so it was put back in this folder and not \
                                     deleted",
                                    pl.path
                                ),
                                Err(e) => {
                                    return Ok(Err(format!(
                                        "{} changed after it was checked, and could not be put back ({e}); it is \
                                         left at .title-select/retained/.deleting/{slot}",
                                        pl.path
                                    )));
                                }
                            };
                            left.push(why.clone());
                            ("left", Some(why))
                        }
                    }
                }
                Some(_) => {
                    return Ok(Err(format!(
                        "what is at .title-select/retained/.deleting/{slot} is not {}; it is left as it is",
                        pl.path
                    )));
                }
            };
            self.fault_point(Fault::ReturnDisposed(seq));
            self.write(|tx| set_placed(tx, p.build, (seq, row.n), state.0, state.1.as_deref()))?;
        }
        self.write(|tx| {
            set_return_state(tx, p.build, seq, Some("done"))?;
            if !left.is_empty() {
                tx.execute(
                    "UPDATE build_items SET error = ?3 WHERE build_id = ?1 AND seq = ?2",
                    params![
                        p.build,
                        seq,
                        format!(
                            "{} is back in source set {}: {}",
                            r.release,
                            p.set_name,
                            left.join("; ")
                        )
                    ],
                )?;
            }
            Ok(())
        })?;
        for pl in r.placed.iter().filter(|pl| !pl.keeps) {
            remove_layout_dir(self, p.dir, p.folder, &pl.path)?;
        }
        Ok(Ok(()))
    }

    /// Rolls back the write of `r`'s release: each placed file set aside put back, then every file written home, or
    /// staged, removed while it is the file recorded; then fails the item with `why`, its release left placed. `Err`
    /// says why a placed file that can't be put back leaves the item outstanding.
    fn unwind(
        &self,
        p: &Returning,
        set: &SetDirs,
        seq: i64,
        r: &PlanReturn,
        why: &str,
    ) -> Result<std::result::Result<(), String>> {
        let rows = self.read(|c| placed_rows(c, p.build, seq))?;
        if rows.iter().any(|row| row.slot.is_some()) {
            let deleting = match open_area_deleting(self, Scope::Library(p.folder), p.dir, false)? {
                Ok(deleting) => deleting,
                Err(e) => {
                    return Ok(Err(format!(
                        "{why}, and the placed files set aside can't be reached to put back ({e})"
                    )));
                }
            };
            for row in rows.iter().filter(|row| row.slot.is_some()) {
                let pl = &r.placed[row.n];
                let slot = row.slot.as_deref().expect("filtered");
                let id = Identity::from(pl.identity);
                let aside = match &deleting {
                    Some(d) => d.stat(slot).map_err(io_err)?,
                    None => None,
                };
                match aside {
                    Some(st) if is_regular(&st, id) => {
                        let deleting = deleting.as_ref().expect("found in it");
                        if let Err(e) = move_back_from(p.dir, pl, deleting, slot) {
                            return Ok(Err(format!(
                                "{why}, and {} could not be put back ({e}); it is left at \
                                 .title-select/retained/.deleting/{slot} until it can be",
                                pl.path
                            )));
                        }
                    }
                    // Never moved, or put back before a stop.
                    None if still_placed(p.dir, pl)? => {}
                    _ => {
                        return Ok(Err(format!(
                            "{why}, and {} is neither in the play library folder nor in its private .deleting \
                             folder; find it and put it back at {}",
                            pl.path, pl.path
                        )));
                    }
                }
                // Moving it changed its change time, which stood for it unread: read whole again where it was
                // placed, holding what it did as it was set aside, it is recorded as found; otherwise it is left
                // recorded as it was, for a check to flag.
                let reread = match &row.sha256 {
                    Some(sha) => reread(p.dir, pl, sha, p.from)?,
                    None => None,
                };
                self.write(|tx| {
                    tx.execute(
                        "UPDATE return_placed SET slot = NULL, moved_ctime_ns = NULL
                         WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                        params![p.build, seq, row.n as i64],
                    )?;
                    if let (Some(st), Some(sha)) = (&reread, &row.sha256) {
                        crate::placements::reread(
                            tx,
                            p.folder,
                            &pl.path,
                            (Identity::from(pl.identity), &pl.file),
                            st,
                            sha,
                        )?;
                    }
                    Ok(())
                })?;
            }
        }
        let outputs = self.read(|c| output_rows(c, p.build, seq))?;
        for (o, f) in outputs.iter().zip(&r.files) {
            let Some(id) = o.id else {
                continue;
            };
            if set
                .dir
                .stat(&f.path)
                .map_err(io_err)?
                .is_some_and(|st| is_regular(&st, id))
            {
                set.dir.unlink(&f.path).map_err(io_err)?;
                set.dir.sync_holding(&f.path).map_err(io_err)?;
                remove_emptied(&set.dir, &f.path);
            }
            if let Some(staged) = &o.staged
                && set
                    .dir
                    .stat(staged)
                    .map_err(io_err)?
                    .is_some_and(|st| is_regular(&st, id))
            {
                set.dir.unlink(staged).map_err(io_err)?;
                set.dir.sync_holding(staged).map_err(io_err)?;
            }
        }
        self.fail_return(p, seq, r, why)
    }

    /// Rolls forward or back each release Build `build`, of plan `plan`, was returning when it stopped, once its
    /// Library Folder `folder` is cleaned up: through the folder, with its marker at the store's generation, and the
    /// Source Set's folder. `Err` says why one is left outstanding.
    pub(super) fn recover_returning(
        &self,
        config: &Config,
        build: i64,
        plan: i64,
        folder: &crate::folders::FolderRow,
        store: String,
    ) -> Result<std::result::Result<(), String>> {
        let open: Vec<(i64, String, String)> = self.read(|c| {
            let rows = c
                .prepare(
                    "SELECT seq, target, return_state FROM build_items
                     WHERE build_id = ?1 AND kind = 'return'
                       AND return_state IN ('renaming', 'writing', 'publishing', 'aside', 'committed')
                     ORDER BY seq",
                )?
                .query_map([build], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })?;
        if open.is_empty() {
            return Ok(Ok(()));
        }
        let dir = match open_writable_folder(config, &folder.root, &folder.path) {
            Ok(dir) => dir,
            Err(why) => return Ok(Err(why)),
        };
        let expected = Marker {
            store,
            folder: folder.id,
            generation: folder.generation,
        };
        if let Err(why) = check_marker(&dir, &expected) {
            return Ok(Err(why));
        }
        if let Err(crate::Error::Conflict(why)) = self.read(|c| {
            crate::lifecycle::check_live(c, RegistrationKind::SourceSet, folder.source_set_id)
        }) {
            return Ok(Err(why));
        }
        let set_dirs = match self.open_placing(config, folder.source_set_id)? {
            Ok(dirs) => dirs,
            Err(why) => {
                return Ok(Err(format!(
                    "a game this build was returning is not finished, and source set {}'s folder can't be opened \
                     to finish it: {why}",
                    folder.source_set
                )));
            }
        };
        let returns = self.plan(plan)?.content.returns;
        let tally = Tally::default();
        let p = Returning {
            build,
            folder: folder.id,
            set: folder.source_set_id,
            set_name: &folder.source_set,
            dir: &dir,
            config,
            to: &tally,
            from: &tally,
        };
        for (seq, release, state) in open {
            let Some(r) = returns.iter().find(|r| r.release == release) else {
                return Ok(Err(format!(
                    "{release} was being returned, but its plan no longer names it"
                )));
            };
            let stopped = format!("the build stopped before {release} was back in the source set");
            let outcome = match state.as_str() {
                "renaming" => self.recover_return_rename(&p, &set_dirs, seq, r)?,
                "committed" => {
                    let deleting =
                        match open_area_deleting(self, Scope::Library(p.folder), p.dir, false)? {
                            Ok(deleting) => deleting,
                            Err(why) => return Ok(Err(why)),
                        };
                    self.dispose_placed(&p, seq, r, deleting.as_ref(), true)?
                }
                _ => self.unwind(&p, &set_dirs, seq, r, &stopped)?,
            };
            if let Err(why) = outcome {
                return Ok(Err(why));
            }
        }
        Ok(Ok(()))
    }

    /// Finishes the renames of `r` a stop left part way. Once every placed file is home, below the folders recorded,
    /// the release is recorded home; otherwise each one moved goes back. `Err` says why it is left outstanding: a
    /// file neither where it was nor home.
    fn recover_return_rename(
        &self,
        p: &Returning,
        set: &SetDirs,
        seq: i64,
        r: &PlanReturn,
    ) -> Result<std::result::Result<(), String>> {
        let Some(pairs) = Self::pairs(r) else {
            return Ok(Err(format!(
                "{} was being moved home, but its plan moves no one file for each",
                r.release
            )));
        };
        let outputs = self.read(|c| output_rows(c, p.build, seq))?;
        let mut homes = Vec::new();
        let mut moved = Vec::new();
        let mut placed = 0;
        for (i, &(n, k)) in pairs.iter().enumerate() {
            let pl = &r.placed[k];
            let f = &r.files[n];
            let found =
                recorded_home(&set.dir, &f.path, outputs[n].chain.as_deref()).map_err(io_err)?;
            let at_home = match &found {
                Ok(h) => h
                    .dir
                    .stat(&h.leaf)
                    .map_err(io_err)?
                    .is_some_and(|st| is_regular(&st, Identity::from(pl.identity))),
                Err(_) => false,
            };
            match (still_placed(p.dir, pl)?, at_home, found) {
                (true, _, found) => {
                    placed += 1;
                    homes.push(found.ok());
                }
                (false, true, Ok(h)) => {
                    moved.push(i);
                    homes.push(Some(h));
                }
                (false, _, found) => {
                    let why = found
                        .err()
                        .unwrap_or_else(|| format!("it is not at {}", f.path));
                    return Ok(Err(format!(
                        "{} is no longer in the play library folder, and {why}; it is left as it is until it is back \
                         in the folder or home, and the build finishes then",
                        pl.path
                    )));
                }
            }
        }
        if placed == 0 {
            // Each home is taken as the file the plan found only while it has the change time recorded right after
            // its rename; one moved with none recorded, as a stop just after it leaves, is not proven, and goes back.
            let rows = self.read(|c| placed_rows(c, p.build, seq))?;
            let mut found = Vec::new();
            let mut changed = None;
            for (&(_, k), h) in pairs.iter().zip(&homes) {
                let pl = &r.placed[k];
                let h = h.as_ref().expect("each is home");
                let file = h.dir.open_file(&h.leaf).map_err(io_err)?;
                let now = rustix::fs::fstat(&file).map_err(io_err)?;
                let moved_ctime = rows
                    .iter()
                    .find(|row| row.n == k)
                    .and_then(|row| row.moved_ctime);
                if Identity::of(&now) != Identity::from(pl.identity)
                    || file_stat(&now) != pl.file
                    || Some(ctime_ns(&now)) != moved_ctime
                {
                    changed = Some(format!("{} changed as it was moved", pl.path));
                    break;
                }
                found.push(now);
            }
            if changed.is_none() {
                self.commit_rename(p, seq, r, &pairs, &found)?;
                return Ok(Ok(()));
            }
        }
        for &i in &moved {
            let (n, k) = pairs[i];
            let pl = &r.placed[k];
            let h = homes[i].as_ref().expect("moved home");
            if let Err(e) = self.unrename_one(p, (seq, k), pl, h) {
                return Ok(Err(format!(
                    "the build stopped before every file of {} was moved home, and {} could not be put back ({e}); \
                     it is at {} until it can be",
                    r.release,
                    pl.path,
                    h.dir.path().join(&h.leaf).display()
                )));
            }
            self.reverify(p, (seq, k), pl, &r.files[n])?;
        }
        // Each put back before the stop, but not yet verified and recorded as found, is now.
        let rows = self.read(|c| placed_rows(c, p.build, seq))?;
        for row in rows.iter().filter(|row| row.state == "putting_back") {
            let pl = &r.placed[row.n];
            if !still_placed(p.dir, pl)? {
                continue;
            }
            if let Some(&(n, _)) = pairs.iter().find(|&&(_, k)| k == row.n) {
                self.reverify(p, (seq, row.n), pl, &r.files[n])?;
            }
        }
        self.fail_return(
            p,
            seq,
            r,
            "the build stopped before its files were all moved home and checked, so those moved were put back",
        )
    }
}

/// Whether the placed file `pl`, set aside as `slot` in `deleting`, is still the file read: the same file, size and
/// modification time, the change time `ctime` read through the descriptor held as it was set aside, and, read whole
/// through its own descriptor while that holds, the SHA-256 `sha`.
fn unchanged(
    deleting: &FolderDir,
    slot: &str,
    pl: &ReturnPlaced,
    ctime: i128,
    sha: &str,
    tally: &Tally,
) -> Result<bool> {
    let file = match deleting.open_file(slot) {
        Ok(file) => file,
        Err(_) => return Ok(false),
    };
    let st = rustix::fs::fstat(&file).map_err(io_err)?;
    Ok(Identity::of(&st) == Identity::from(pl.identity)
        && file_stat(&st) == pl.file
        && ctime_ns(&st) == ctime
        && still_sha256(&file, ctime, tally)
            .map_err(io_err)?
            .is_some_and(|read| read == sha))
}

/// The placed file `pl`, back at its name in the Library Folder `dir`, as statted, while it is the file the plan found,
/// its size and modification time unchanged, and, read whole through its own descriptor while that holds, the
/// SHA-256 `sha` read as it was set aside. None when it isn't.
fn reread(dir: &FolderDir, pl: &ReturnPlaced, sha: &str, tally: &Tally) -> Result<Option<Stat>> {
    let Ok(file) = dir.open_file(&pl.path) else {
        return Ok(None);
    };
    let st = rustix::fs::fstat(&file).map_err(io_err)?;
    if !is_regular(&st, Identity::from(pl.identity)) || file_stat(&st) != pl.file {
        return Ok(None);
    }
    let read = still_sha256(&file, ctime_ns(&st), tally).map_err(io_err)?;
    Ok(read.is_some_and(|read| read == sha).then_some(st))
}

/// Renames what is at `leaf` in `from` back to `pl`'s place in the Library Folder `dir`, making its folders again,
/// without following a symbolic link, if they are gone; never replacing anything.
fn move_back(dir: &FolderDir, pl: &ReturnPlaced, from: &FolderDir, leaf: &str) -> io::Result<()> {
    let (parent, name) = parent_made(dir, &pl.path)?;
    let at = parent.as_ref().unwrap_or(dir);
    from.rename_new(leaf, at, name)?;
    from.sync()?;
    at.sync()
}

/// Puts the placed file `pl`, set aside as `slot` in `deleting`, back where it was, as [`move_back`].
fn move_back_from(
    dir: &FolderDir,
    pl: &ReturnPlaced,
    deleting: &FolderDir,
    slot: &str,
) -> io::Result<()> {
    move_back(dir, pl, deleting, slot)
}

/// The Source Set's latest finished Audit, which a return makes name the files it put home.
fn latest_audit(tx: &Transaction, set: i64) -> Result<Option<i64>> {
    Ok(tx
        .query_row(
            "SELECT id FROM audits WHERE source_set_id = ?1 AND status = 'finished' AND pruned_at IS NULL
             ORDER BY finish_seq DESC LIMIT 1",
            [set],
            |r| r.get(0),
        )
        .optional()?)
}
