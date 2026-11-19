//! Placing Games (ADR 0023): a Build item of kind `place` makes its Game's file in the Library Folder holding its
//! Source Set's Placed Games that Game's only copy, and so changes the Source Set.
//!
//! - **Rename**: the Source Set's file is the file the folder stores, on the same filesystem. It is held open, checked
//!   to be the file the plan found by its device, inode, size, modification time and change time, and renamed into
//!   place by a rename that never replaces anything. It is checked again through the descriptor held across the
//!   rename, and its new change time read through it is recorded: it is never read, and its Audit's Entries and the
//!   payload read an Incremental Audit reuses follow it, their Evidence carried over.
//! - **Copy**: the same file on another filesystem, or where a rename can't reach. It is copied under a staged name,
//!   read back against what the original read as while it held still, and published by a rename that never replaces
//!   anything ([`crate::carry::copy`]).
//! - **Convert**: the folder stores the Game in another format. Each Source Set file it is written from is read whole
//!   while it holds still and recorded first; the new file is written and checked in the staging folder as any built
//!   file is, and published the same way.
//! - **Built**: the folder holds the Game's file already, as built. The Source Set's files are read and recorded as for
//!   a conversion, and the built file is read again to check it still holds what it was built as.
//!
//! The Game is recorded placed, its file recorded built, and its Source Set's Audit made to name it there, in the one
//! transaction that ends the step that put it in place, and only for the file found there. Only then is each Source
//! Set file it consumed deleted, under ADR 0010's trusted-stamp rule ([`crate::carry::dispose_as`]): set aside into the
//! Source Set's private `.deleting` folder through a descriptor held across that rename, read back, and deleted only
//! while it still reads as recorded and the placed file, held open, still reads as it was placed. One that can't be
//! proven unchanged is put back and kept in the Source Set. So at every step the Game has a recorded copy: its
//! Source Set's file until it is placed, then its placed file. Each step is recorded before it is taken, and recovery
//! rolls it forward or back; one it can't finish is left outstanding, with why, for the next start.

use std::collections::HashMap;
use std::fs::File;
use std::time::SystemTime;

use rusqlite::{OptionalExtension, Transaction, params};
use rustix::fs::{FileType, Stat};
use title_select_api::build::{
    FileIdentity, ItemState, Marker, PlaceHow, PlacedOriginal, PlanItem, PlanPlace,
};
use title_select_api::lifecycle::RegistrationKind;
use title_select_api::storage::placed_path;
use title_select_engine::meter::Tally;

use super::{
    Failure, Staged, Work, is_file, open_source, record_applied, record_temp, remove_staged,
    set_item, stage_by_fd, staged_temp, still_sha256,
};
use crate::carry::{self, Intent, Intents};
use crate::config::Config;
use crate::folder_dir::{FolderDir, Identity, ctime_ns, file_stat, parent_of};
use crate::folders::check_marker;
use crate::kept::{Scope, open_area_deleting, settle_dir_temps};
use crate::repairs::{MarkerRule, open_repairable, source_folder};
use crate::{Error, Fault, Result, Store};

fn io_err(e: impl std::fmt::Display) -> Error {
    Error::Io(e.to_string())
}

/// The Source Set's folder a placing Build changes, opened to write to, and its private `.deleting` folder.
pub(super) struct SetDirs {
    pub dir: FolderDir,
    pub deleting: FolderDir,
}

/// What placing an item needs of its Build.
pub(super) struct Placing<'a> {
    pub build: i64,
    pub folder: i64,
    pub set: i64,
    pub set_name: &'a str,
    /// The Source Set's Audit the plan was made from, which is made to name each placed file where it is.
    pub audit: i64,
    /// The Library Folder, opened to write to, and its marker as the Build found it.
    pub dir: &'a FolderDir,
    pub expected: &'a Marker,
    /// Where what is read and written in the Library Folder's Root is counted, and in the Source Set's.
    pub to: &'a Tally,
    pub from: &'a Tally,
}

/// A placing item a stop left part way: its seq, target, placement state and item state.
type Stopped = (i64, String, String, String);

/// A Source Set file a placing item recorded before it changed anything: as the plan found it, and what it read as.
struct OriginalRow {
    n: i64,
    original: PlacedOriginal,
    sha256: Option<String>,
}

fn original_rows(c: &rusqlite::Connection, build: i64, seq: i64) -> Result<Vec<OriginalRow>> {
    let mut stmt = c.prepare(
        "SELECT n, path, size, mtime_ns, dev, ino, ctime_ns, sha256 FROM placement_originals
         WHERE build_id = ?1 AND seq = ?2 AND state = 'pending' ORDER BY n",
    )?;
    let rows = stmt.query_map(params![build, seq], |r| {
        Ok(OriginalRow {
            n: r.get(0)?,
            original: PlacedOriginal {
                path: r.get(1)?,
                file: title_select_api::build::FileStat {
                    size: r.get::<_, i64>(2)? as u64,
                    mtime_ns: r.get::<_, i64>(3)? as i128,
                },
                identity: FileIdentity {
                    dev: r.get::<_, i64>(4)? as u64,
                    ino: r.get::<_, i64>(5)? as u64,
                },
                ctime_ns: r.get::<_, i64>(6)? as i128,
            },
            sha256: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn insert_original(
    tx: &Transaction,
    (build, seq, n): (i64, i64, i64),
    o: &PlacedOriginal,
    sha256: Option<&str>,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO placement_originals
             (build_id, seq, n, path, size, mtime_ns, dev, ino, ctime_ns, sha256, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending')",
        params![
            build,
            seq,
            n,
            o.path,
            o.file.size as i64,
            o.file.mtime_ns as i64,
            o.identity.dev as i64,
            o.identity.ino as i64,
            o.ctime_ns as i64,
            sha256
        ],
    )?;
    Ok(())
}

fn set_place_state(tx: &Transaction, build: i64, seq: i64, state: Option<&str>) -> Result<()> {
    tx.execute(
        "UPDATE build_items SET place_state = ?3 WHERE build_id = ?1 AND seq = ?2",
        params![build, seq, state],
    )?;
    Ok(())
}

/// Where the copy of a placing item's one Source Set file records its [`Intent`]: on the item's row.
struct CopyIntents<'a> {
    store: &'a Store,
    build: i64,
    seq: i64,
}

impl Intents for CopyIntents<'_> {
    fn load(&self) -> Result<Intent> {
        self.store.read(|c| {
            Ok(c.query_row(
                "SELECT copy_staged, copy_dev, copy_ino, copy_sha256 FROM build_items
                 WHERE build_id = ?1 AND seq = ?2",
                params![self.build, self.seq],
                |r| {
                    let staged_id = match (r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?)
                    {
                        (Some(dev), Some(ino)) => Some(Identity::from_sql(dev, ino)),
                        _ => None,
                    };
                    Ok(Intent {
                        staged: r.get(0)?,
                        staged_id,
                        sha256: r.get(3)?,
                        slot: None,
                        moved_ctime: None,
                    })
                },
            )?)
        })
    }

    fn save(&self, intent: &Intent) -> Result<()> {
        let (dev, ino) = intent.staged_id.map(Identity::to_sql).unzip();
        self.store.write(|tx| {
            tx.execute(
                "UPDATE build_items SET copy_staged = ?3, copy_dev = ?4, copy_ino = ?5, copy_sha256 = ?6
                 WHERE build_id = ?1 AND seq = ?2",
                params![
                    self.build,
                    self.seq,
                    intent.staged,
                    dev,
                    ino,
                    intent.sha256
                ],
            )?;
            Ok(())
        })
    }
}

/// Where the deletion of a placing item's Source Set file `n` records its [`Intent`]: its slot in the private
/// `.deleting` folder on its own row; the placed file it stands in for, `published`, and what the original read as,
/// are recorded already.
struct DisposeIntents<'a> {
    store: &'a Store,
    build: i64,
    seq: i64,
    n: i64,
    published: Identity,
}

impl Intents for DisposeIntents<'_> {
    fn load(&self) -> Result<Intent> {
        self.store.read(|c| {
            Ok(c.query_row(
                "SELECT sha256, slot, moved_ctime_ns FROM placement_originals
                 WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                params![self.build, self.seq, self.n],
                |r| {
                    Ok(Intent {
                        staged: None,
                        staged_id: Some(self.published),
                        sha256: r.get(0)?,
                        slot: r.get(1)?,
                        moved_ctime: r
                            .get::<_, Option<String>>(2)?
                            .and_then(|ns| ns.parse().ok()),
                    })
                },
            )?)
        })
    }

    fn save(&self, intent: &Intent) -> Result<()> {
        self.store.write(|tx| {
            tx.execute(
                "UPDATE placement_originals SET slot = ?4, moved_ctime_ns = ?5
                 WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                params![
                    self.build,
                    self.seq,
                    self.n,
                    intent.slot,
                    intent.moved_ctime.map(|ns| ns.to_string())
                ],
            )?;
            Ok(())
        })
    }

    fn verifying(&self) {
        self.store.fault_point(Fault::PlaceChecking(self.seq));
    }
}

/// How a placed file is recorded built, in the transaction that records it placed.
enum Built {
    /// Put in place without a publish intent, by a rename or a copy: its stamp, as found, is its intent.
    Found,
    /// Published from the staging folder: recorded only while it is the file its publish intent names.
    Published,
    /// Recorded built already, by the Build or by its recovery.
    Recorded,
    /// Built by an earlier Build: its stamp is recorded for the item, and nothing else of it changes.
    Earlier,
}

/// How the Source Set's Audit is made to name a placed file.
enum Entries<'a> {
    /// Moved as it was, renamed or copied: its Entries follow it, as `was` found it.
    Moved(&'a PlacedOriginal),
    /// Converted from, or stood in for by a built file: each ROM's Entry is now of it.
    Rewritten,
}

/// The file `rel` in `dir`, held open while it is the regular file `o`, as the plan found it: the same device and inode,
/// size, modification time and change time. Returns why not.
fn hold(dir: &FolderDir, o: &PlacedOriginal) -> Result<std::result::Result<File, String>> {
    let (parent, name) = match parent_of(dir, &o.path) {
        Ok(found) => found,
        Err(e) => return Ok(Err(format!("{} can't be reached: {e}", o.path))),
    };
    let Some(file) = parent
        .as_ref()
        .unwrap_or(dir)
        .pin(name, Identity::from(o.identity))
    else {
        return Ok(Err(format!(
            "{} is no longer the file the plan found",
            o.path
        )));
    };
    let st = rustix::fs::fstat(&file).map_err(io_err)?;
    if file_stat(&st) != o.file || ctime_ns(&st) != o.ctime_ns {
        return Ok(Err(format!("{} changed since the plan was made", o.path)));
    }
    Ok(Ok(file))
}

impl Store {
    /// Opens Source Set `set`'s folder for a Build to place its Games from, as a repair opens it: only while it is
    /// registered, its Root lets Title Select write there, and its marker is this store's, written first if no repair
    /// wrote one. Returns why not.
    pub(super) fn open_placing(
        &self,
        config: &Config,
        set: i64,
    ) -> Result<std::result::Result<SetDirs, String>> {
        if let Err(Error::Conflict(why)) =
            self.read(|c| crate::lifecycle::check_live(c, RegistrationKind::SourceSet, set))
        {
            return Ok(Err(why));
        }
        // Its marker is recorded where a repair records it, whichever writes it first.
        self.write(|tx| {
            tx.execute(
                "INSERT INTO source_folders (source_set_id, generation) VALUES (?1, 0)
                 ON CONFLICT (source_set_id) DO NOTHING",
                [set],
            )?;
            Ok(())
        })?;
        let (_, marked) = self.read(|c| source_folder(c, set))?;
        let rule = match marked {
            true => MarkerRule::Required,
            false => MarkerRule::Creating,
        };
        let dir = match open_repairable(self, config, set, rule)? {
            Ok(dir) => dir,
            Err(why) => return Ok(Err(why)),
        };
        if !marked && let Err(why) = self.bootstrap_marker(set, &dir)? {
            return Ok(Err(why));
        }
        if let Err(why) = settle_dir_temps(self, Scope::Source(set), &dir)? {
            return Ok(Err(why));
        }
        let deleting = match open_area_deleting(self, Scope::Source(set), &dir, true)? {
            Ok(Some(deleting)) => deleting,
            Ok(None) => return Ok(Err("its private .deleting folder is missing".into())),
            Err(why) => return Ok(Err(why)),
        };
        Ok(Ok(SetDirs { dir, deleting }))
    }

    /// Places the Game of `w`, an item of kind `place`, as its plan says. Each failure that leaves the Game as it was,
    /// in its Source Set, fails only the item; one that leaves anything to finish stops the Build, for recovery.
    pub(super) fn place(
        &self,
        p: &Placing,
        set: &SetDirs,
        w: &Work,
        (staging, staged_ids, built_at): (&FolderDir, &mut HashMap<String, Identity>, SystemTime),
    ) -> Result<()> {
        let place = w
            .item
            .place
            .as_ref()
            .ok_or_else(|| Error::Io(format!("{} has no placement", w.item.target)))?;
        check_marker(p.dir, p.expected).map_err(Error::Refused)?;
        match place.how {
            PlaceHow::Rename => self.place_rename(p, set, w, place),
            PlaceHow::Copy => self.place_copy(p, set, w, place),
            PlaceHow::Convert => {
                self.place_convert(p, set, w, place, (staging, staged_ids, built_at))
            }
            PlaceHow::Built => self.place_built(p, set, w, place),
        }
    }

    /// Fails `w` with `why`, its Game left in its Source Set as it was.
    fn place_failed(&self, p: &Placing, w: &Work, why: &str) -> Result<()> {
        self.write(|tx| {
            set_place_state(tx, p.build, w.seq, None)?;
            set_item(
                tx,
                p.build,
                w.seq,
                ItemState::Failed,
                &format!(
                    "{why}; {} stays in source set {} as it was",
                    w.item.game, p.set_name
                ),
            )
        })
    }

    fn place_rename(&self, p: &Placing, set: &SetDirs, w: &Work, place: &PlanPlace) -> Result<()> {
        let [o] = place.originals.as_slice() else {
            return self.place_failed(p, w, "a rename moves one file");
        };
        let (parent, name) = parent_of(&set.dir, &o.path).map_err(io_err)?;
        let from = parent.as_ref().unwrap_or(&set.dir);
        let held = match hold(&set.dir, o)? {
            Ok(held) => held,
            Err(why) => return self.place_failed(p, w, &why),
        };
        self.write(|tx| set_place_state(tx, p.build, w.seq, Some("renaming")))?;
        self.fault_point(Fault::PlaceRenaming(w.seq));
        let target = &w.item.target;
        if let Err(e) = from.rename_new(name, p.dir, target) {
            drop(held);
            if self.across_filesystems(Some(&e)) {
                // No rename reaches the folder from here: it is copied instead.
                self.write(|tx| set_place_state(tx, p.build, w.seq, None))?;
                return self.place_copy(p, set, w, place);
            }
            return self.place_failed(
                p,
                w,
                &format!("{} could not be moved into the folder: {e}", o.path),
            );
        }
        self.fault_point(Fault::PlaceMoved(w.seq));
        match self.finish_rename(p, set, w, o, Some(&held))? {
            Ok(()) => Ok(()),
            Err(why) => Err(Error::Refused(why)),
        }
    }

    /// Ends the rename of `o`, the Source Set file of `w`, into the folder, now or after a stop: both folders synced,
    /// and `o`, at its target, recorded placed as it is through `held`, the descriptor held across the rename, or one
    /// opened on it now. What is at its target is put back unless it is `o`, unchanged. `Err` says why it is left
    /// outstanding.
    fn finish_rename(
        &self,
        p: &Placing,
        set: &SetDirs,
        w: &Work,
        o: &PlacedOriginal,
        held: Option<&File>,
    ) -> Result<std::result::Result<(), String>> {
        let target = &w.item.target;
        let (parent, name) = parent_of(&set.dir, &o.path).map_err(io_err)?;
        let from = parent.as_ref().unwrap_or(&set.dir);
        from.sync().map_err(io_err)?;
        p.dir.sync().map_err(io_err)?;
        let id = Identity::from(o.identity);
        let at = p.dir.stat(target).map_err(io_err)?;
        let moved = at.as_ref().is_some_and(|st| {
            FolderDir::kind(st) == FileType::RegularFile && Identity::of(st) == id
        });
        if !moved {
            let home = from
                .stat(name)
                .map_err(io_err)?
                .is_some_and(|st| Identity::of(&st) == id);
            if home {
                self.place_failed(p, w, &format!("{} was not moved", o.path))?;
                return Ok(Ok(()));
            }
            // Renamed here, but not the original: another file took its name first. It goes back.
            if held.is_some() {
                return match p.dir.rename_new(target, from, name) {
                    Ok(()) => {
                        p.dir.sync().map_err(io_err)?;
                        from.sync().map_err(io_err)?;
                        self.place_failed(
                            p,
                            w,
                            &format!(
                                "{} was replaced as it was moved, so what replaced it was put back",
                                o.path
                            ),
                        )?;
                        Ok(Ok(()))
                    }
                    Err(e) => Ok(Err(format!(
                        "{} was replaced as it was moved into the folder, and what replaced it could not be put \
                         back ({e}); it is at {} until it can be",
                        o.path,
                        p.dir.path().join(target).display()
                    ))),
                };
            }
            // Neither where it was nor where it was going: nothing records a copy of the game, so the placement is
            // left outstanding rather than ended.
            return Ok(Err(format!(
                "{} is neither in source set {} nor at {}; find it and put it back in the source set",
                o.path,
                p.set_name,
                p.dir.path().join(target).display()
            )));
        }
        let opened;
        let held = match held {
            Some(held) => held,
            None => {
                opened = p.dir.open_file(target).map_err(io_err)?;
                &opened
            }
        };
        let now = rustix::fs::fstat(held).map_err(io_err)?;
        if Identity::of(&now) != id || file_stat(&now) != o.file {
            // It changed on the way: back where it was, unplaced, or outstanding until it can be.
            return match p.dir.rename_new(target, from, name) {
                Ok(()) => {
                    p.dir.sync().map_err(io_err)?;
                    from.sync().map_err(io_err)?;
                    self.place_failed(
                        p,
                        w,
                        &format!("{} changed as it was moved, so it was put back", o.path),
                    )?;
                    Ok(Ok(()))
                }
                Err(e) => Ok(Err(format!(
                    "{} changed as it was moved into the folder, and could not be put back ({e}); it is at {} \
                     until it can be",
                    o.path,
                    p.dir.path().join(target).display()
                ))),
            };
        }
        let st = at.expect("checked above");
        self.record_placed(
            p,
            w,
            (&st, &now),
            None,
            Entries::Moved(o),
            "done",
            Built::Found,
        )?;
        Ok(Ok(()))
    }

    fn place_copy(&self, p: &Placing, set: &SetDirs, w: &Work, place: &PlanPlace) -> Result<()> {
        let [o] = place.originals.as_slice() else {
            return self.place_failed(p, w, "a copy is of one file");
        };
        self.write(|tx| {
            insert_original(tx, (p.build, w.seq, 0), o, None)?;
            set_place_state(tx, p.build, w.seq, Some("copying"))
        })?;
        match self.finish_copy(p, set, w, o)? {
            Ok(()) => Ok(()),
            Err(why) => Err(Error::Refused(why)),
        }
    }

    /// Copies `o`, the Source Set file of `w`, into the folder, or finishes the copy a stop left: published, checked
    /// to hold what `o` read as, recorded placed, then `o` deleted. `Err` says why it is left outstanding.
    fn finish_copy(
        &self,
        p: &Placing,
        set: &SetDirs,
        w: &Work,
        o: &PlacedOriginal,
    ) -> Result<std::result::Result<(), String>> {
        let (parent, name) = match parent_of(&set.dir, &o.path) {
            Ok(found) => found,
            Err(e) => {
                self.place_failed(p, w, &format!("{} can't be reached: {e}", o.path))?;
                return Ok(Ok(()));
            }
        };
        let original = carry::Original {
            dir: parent.as_ref().unwrap_or(&set.dir),
            name,
            label: &o.path,
            id: Identity::from(o.identity),
            file: o.file.clone(),
            ctime: Some(o.ctime_ns),
            sha256: None,
            deleting: &set.deleting,
            tally: p.from,
        };
        let dest = carry::Dest {
            root: p.dir,
            dir: p.dir,
            name: &w.item.target,
            tally: p.to,
        };
        let intents = CopyIntents {
            store: self,
            build: p.build,
            seq: w.seq,
        };
        let st = match carry::copy(&original, &dest, &intents)? {
            Ok(st) => st,
            Err(why) => {
                self.place_failed(p, w, &why)?;
                return Ok(Ok(()));
            }
        };
        self.fault_point(Fault::PlaceCopied(w.seq));
        // Checked again before it stands in for the original, here or after a stop.
        if let Err(why) = carry::published_holds(&dest, &intents)? {
            self.place_failed(
                p,
                w,
                &format!("{} was copied into the folder, but {why}", o.path),
            )?;
            return Ok(Ok(()));
        }
        let intent = intents.load()?;
        let sha = intent.sha256.as_deref();
        self.write(|tx| {
            tx.execute(
                "UPDATE placement_originals SET sha256 = ?3 WHERE build_id = ?1 AND seq = ?2 AND n = 0",
                params![p.build, w.seq, sha],
            )?;
            Ok(())
        })?;
        self.record_placed(
            p,
            w,
            (&st, &st),
            sha,
            Entries::Moved(o),
            "disposing",
            Built::Found,
        )?;
        self.fault_point(Fault::PlaceRecorded(w.seq));
        self.dispose_originals(p, set, w)
    }

    /// Reads each Source Set file `w` is placed from whole, while it holds still as the plan found it, and records
    /// what it read as before anything is written for it. Returns why not.
    fn record_originals(
        &self,
        p: &Placing,
        set: &SetDirs,
        w: &Work,
        place: &PlanPlace,
    ) -> Result<std::result::Result<(), String>> {
        let mut read = Vec::new();
        for o in &place.originals {
            let held = match hold(&set.dir, o)? {
                Ok(held) => held,
                Err(why) => return Ok(Err(why)),
            };
            match still_sha256(&held, o.ctime_ns, p.from).map_err(io_err)? {
                Some(sha) => read.push(sha),
                None => return Ok(Err(format!("{} changed while it was read", o.path))),
            }
        }
        self.write(|tx| {
            for (n, (o, sha)) in place.originals.iter().zip(&read).enumerate() {
                insert_original(tx, (p.build, w.seq, n as i64), o, Some(sha))?;
            }
            set_place_state(tx, p.build, w.seq, Some("converting"))
        })?;
        Ok(Ok(()))
    }

    fn place_convert(
        &self,
        p: &Placing,
        set: &SetDirs,
        w: &Work,
        place: &PlanPlace,
        (staging, staged_ids, built_at): (&FolderDir, &mut HashMap<String, Identity>, SystemTime),
    ) -> Result<()> {
        if let Err(why) = self.record_originals(p, set, w, place)? {
            return self.place_failed(p, w, &why);
        }
        let name = format!("{}.zip", w.seq);
        let mut file = staging.create_new(&name).map_err(io_err)?;
        let file_id = Identity::of_fd(&file).map_err(io_err)?;
        record_temp(self, p.folder, &staged_temp(&name), file_id)?;
        staged_ids.insert(name.clone(), file_id);
        let staged = match stage_placed(w.item, &mut file, &set.dir, (p.from, p.to), built_at) {
            Ok(staged) => staged,
            Err(Failure::Folder(why)) => return Err(Error::Io(why)),
            Err(Failure::Item(why)) => {
                drop(file);
                remove_staged(staging, &name, file_id).map_err(Error::Refused)?;
                return self.place_failed(p, w, &why);
            }
            Err(Failure::Cancelled) => {
                drop(file);
                remove_staged(staging, &name, file_id).map_err(Error::Refused)?;
                return self.place_failed(p, w, "you cancelled the build");
            }
        };
        self.fault_point(Fault::Staged(w.seq));
        let (dev, ino) = file_id.to_sql();
        self.write(|tx| {
            tx.execute(
                "UPDATE build_items SET state = 'publishing', sha256 = ?3, size = ?4, dev = ?5, ino = ?6,
                                        mtime_ns = ?7
                 WHERE build_id = ?1 AND seq = ?2",
                params![
                    p.build,
                    w.seq,
                    staged.sha256,
                    staged.size as i64,
                    dev,
                    ino,
                    staged.mtime_ns as i64
                ],
            )?;
            Ok(())
        })?;
        self.fault_point(Fault::BeforePublish(w.seq));
        let target = &w.item.target;
        let renamed = check_marker(p.dir, p.expected)
            .and_then(|()| match is_file(staging, &name, file_id)? {
                true => Ok(()),
                false => Err(format!(
                    "{} was replaced after it was verified; it was left as it is",
                    staging.path().join(&name).display()
                )),
            })
            .and_then(|()| {
                staging
                    .rename_new(&name, p.dir, target)
                    .map_err(|e| match e.kind() {
                        std::io::ErrorKind::AlreadyExists => format!(
                            "{target} appeared in the folder during the build; it was left as it is"
                        ),
                        _ => e.to_string(),
                    })
            });
        if let Err(why) = renamed {
            self.place_failed(p, w, &why)?;
            return Err(Error::Refused(why));
        }
        self.fault_point(Fault::PublishedUnsynced(w.seq));
        staging.sync().map_err(io_err)?;
        p.dir.sync().map_err(io_err)?;
        self.fault_point(Fault::AfterPublish(w.seq));
        drop(file);
        match self.finish_convert(p, set, w, true)? {
            Ok(()) => Ok(()),
            Err(why) => Err(Error::Refused(why)),
        }
    }

    /// Records `w`'s converted file, published at its target, placed, once it is the file the Build verified, then
    /// deletes the Source Set files it was converted from; now or after a stop, when `record` says it is not yet
    /// recorded built. `Err` says why it is left outstanding.
    fn finish_convert(
        &self,
        p: &Placing,
        set: &SetDirs,
        w: &Work,
        record: bool,
    ) -> Result<std::result::Result<(), String>> {
        let (dev, ino, sha): (Option<i64>, Option<i64>, Option<String>) = self.read(|c| {
            Ok(c.query_row(
                "SELECT dev, ino, sha256 FROM build_items WHERE build_id = ?1 AND seq = ?2",
                params![p.build, w.seq],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })?;
        let (Some(dev), Some(ino), Some(sha)) = (dev, ino, sha) else {
            self.place_failed(p, w, "nothing it wrote is recorded")?;
            return Ok(Ok(()));
        };
        let id = Identity::from_sql(dev, ino);
        let target = &w.item.target;
        let st = match p.dir.stat(target).map_err(io_err)? {
            Some(st)
                if FolderDir::kind(&st) == FileType::RegularFile && Identity::of(&st) == id =>
            {
                st
            }
            _ => {
                self.write(|tx| {
                    set_place_state(tx, p.build, w.seq, None)?;
                    set_item(
                        tx,
                        p.build,
                        w.seq,
                        ItemState::Uncertain,
                        &format!(
                            "{target} is not the file the build put there; it was left as it is, and {} stays in \
                             source set {}",
                            w.item.game, p.set_name
                        ),
                    )
                })?;
                return Ok(Ok(()));
            }
        };
        self.record_placed(
            p,
            w,
            (&st, &st),
            Some(&sha),
            Entries::Rewritten,
            "disposing",
            match record {
                true => Built::Published,
                false => Built::Recorded,
            },
        )?;
        self.fault_point(Fault::PlaceRecorded(w.seq));
        self.dispose_originals(p, set, w)
    }

    fn place_built(&self, p: &Placing, set: &SetDirs, w: &Work, place: &PlanPlace) -> Result<()> {
        let target = &w.item.target;
        let built: Option<(Option<String>, i64, i64)> = self.read(|c| {
            Ok(c.query_row(
                "SELECT sha256, size, mtime_ns FROM built_files WHERE folder_id = ?1 AND target = ?2",
                params![p.folder, target],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?)
        })?;
        let Some((Some(sha), size, mtime_ns)) = built else {
            return self.place_failed(
                p,
                w,
                &format!("{target} has no record of what it was built as"),
            );
        };
        let st = match p.dir.stat(target).map_err(io_err)? {
            Some(st)
                if FolderDir::kind(&st) == FileType::RegularFile
                    && file_stat(&st).size == size as u64
                    && file_stat(&st).mtime_ns == mtime_ns as i128 =>
            {
                st
            }
            _ => return self.place_failed(p, w, &format!("{target} changed since it was built")),
        };
        if let Err(why) = self.record_originals(p, set, w, place)? {
            return self.place_failed(p, w, &why);
        }
        // Read again before it stands in for its Source Set's files: still the file built, as built.
        let held = match carry::Held::open(p.dir, target, Identity::of(&st))? {
            Ok(held) => held,
            Err(why) => return self.place_failed(p, w, &format!("{target}: {why}")),
        };
        if let Err(why) = held.still(p.dir, target, &sha, p.to)? {
            return self.place_failed(p, w, &format!("{target}: {why}"));
        }
        self.record_placed(
            p,
            w,
            (&st, &st),
            Some(&sha),
            Entries::Rewritten,
            "disposing",
            Built::Earlier,
        )?;
        self.fault_point(Fault::PlaceRecorded(w.seq));
        match self.dispose_originals(p, set, w)? {
            Ok(()) => Ok(()),
            Err(why) => Err(Error::Refused(why)),
        }
    }

    /// Records `w`'s Game placed as its file at its target, found there as `at` and statted through a descriptor as
    /// `now`, read as `sha` if it was read: its file recorded built, the Game placed, its Source Set's Audit made to
    /// Records `w`'s Game placed as its file at its target, found there as `at` and statted through a descriptor as
    /// `now`, read as `sha` if it was read: its file recorded built as `built` says, the Game placed, its Source
    /// Set's Audit made to name it there, and the item applied, with its placement at `next`, in one transaction.
    #[allow(clippy::too_many_arguments)]
    fn record_placed(
        &self,
        p: &Placing,
        w: &Work,
        (at, now): (&Stat, &Stat),
        sha: Option<&str>,
        entries: Entries,
        next: &str,
        built: Built,
    ) -> Result<()> {
        let item = w.item;
        let place = item.place.as_ref().expect("a placing item");
        let path = placed_path(p.folder, &item.target);
        let (dev, ino) = Identity::of(at).to_sql();
        let file = file_stat(at);
        self.write(|tx| {
            let stamp = |tx: &Transaction| -> Result<()> {
                tx.execute(
                    "UPDATE build_items SET sha256 = ?3, size = ?4, dev = ?5, ino = ?6, mtime_ns = ?7
                     WHERE build_id = ?1 AND seq = ?2",
                    params![
                        p.build,
                        w.seq,
                        sha,
                        file.size as i64,
                        dev,
                        ino,
                        file.mtime_ns as i64
                    ],
                )?;
                Ok(())
            };
            if matches!(built, Built::Found | Built::Earlier) {
                stamp(tx)?;
            }
            match built {
                Built::Found | Built::Published => {
                    if !record_applied(tx, p.build, p.folder, w.seq, item, at, true)? {
                        return Err(Error::Io(format!(
                            "{} is not the file the build put there",
                            item.target
                        )));
                    }
                }
                Built::Recorded | Built::Earlier => {
                    tx.execute(
                        "UPDATE build_items SET state = 'applied', error = NULL WHERE build_id = ?1 AND seq = ?2",
                        params![p.build, w.seq],
                    )?;
                }
            }
            crate::placements::place_file_as(tx, p.set, p.folder, &item.target, &item.game, now, sha)?;
            match entries {
                Entries::Moved(was) => crate::placements::moved_entries(
                    tx,
                    (p.set, p.audit),
                    &was.path,
                    &path,
                    was,
                    now,
                )?,
                Entries::Rewritten => {
                    let from: Vec<&str> = place.originals.iter().map(|o| o.path.as_str()).collect();
                    crate::placements::rewritten_entries(
                        tx,
                        (p.set, p.audit),
                        &from,
                        &path,
                        &item.game,
                        (&item.roms, item.loose),
                    )?
                }
            }
            set_place_state(tx, p.build, w.seq, Some(next))
        })
    }

    /// Deletes each Source Set file `w`'s Game, now placed, was placed from and has not yet disposed of, under the
    /// trusted-stamp rule ([`carry::dispose_as`]). One that can't be proven unchanged is put back and kept in the
    /// Source Set, and the item says so. `Err` says why one is left outstanding: set aside to delete and neither
    /// deleted nor put back.
    fn dispose_originals(
        &self,
        p: &Placing,
        set: &SetDirs,
        w: &Work,
    ) -> Result<std::result::Result<(), String>> {
        let (dev, ino, sha): (Option<i64>, Option<i64>, Option<String>) = self.read(|c| {
            Ok(c.query_row(
                "SELECT dev, ino, sha256 FROM build_items WHERE build_id = ?1 AND seq = ?2",
                params![p.build, w.seq],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })?;
        let (Some(dev), Some(ino), Some(sha)) = (dev, ino, sha) else {
            return Ok(Err(format!(
                "{} is placed, but what it was placed as isn't recorded, so its source set's files are left as \
                 they are",
                w.item.target
            )));
        };
        let published = Identity::from_sql(dev, ino);
        let mut kept = Vec::new();
        for row in self.read(|c| original_rows(c, p.build, w.seq))? {
            let o = &row.original;
            let Some(original_sha) = &row.sha256 else {
                kept.push((
                    row.n,
                    format!("{} was not read, so it stays in the source set", o.path),
                ));
                continue;
            };
            let (parent, name) = match parent_of(&set.dir, &o.path) {
                Ok(found) => found,
                Err(e) => {
                    kept.push((
                        row.n,
                        format!("{} can't be reached ({e}), so it stays as it is", o.path),
                    ));
                    continue;
                }
            };
            let original = carry::Original {
                dir: parent.as_ref().unwrap_or(&set.dir),
                name,
                label: &o.path,
                id: Identity::from(o.identity),
                file: o.file.clone(),
                ctime: Some(o.ctime_ns),
                sha256: Some(original_sha),
                deleting: &set.deleting,
                tally: p.from,
            };
            let dest = carry::Dest {
                root: p.dir,
                dir: p.dir,
                name: &w.item.target,
                tally: p.to,
            };
            let intents = DisposeIntents {
                store: self,
                build: p.build,
                seq: w.seq,
                n: row.n,
                published,
            };
            let outcome = carry::dispose_as(&original, &dest, &intents, Some(&sha))?;
            self.fault_point(Fault::PlaceDisposed(w.seq));
            match outcome {
                Ok(()) => self.write(|tx| {
                    tx.execute(
                        "UPDATE placement_originals SET state = 'deleted' WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                        params![p.build, w.seq, row.n],
                    )?;
                    Ok(())
                })?,
                // Set aside to delete, and not put back: never recorded kept, and never deleted, until it is one or
                // the other.
                Err(why) if intents.load()?.slot.is_some() => return Ok(Err(why)),
                Err(why) => kept.push((row.n, why)),
            }
        }
        self.write(|tx| {
            for (n, why) in &kept {
                tx.execute(
                    "UPDATE placement_originals SET state = 'kept', detail = ?4
                     WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                    params![p.build, w.seq, n, why],
                )?;
            }
            if !kept.is_empty() {
                let whys: Vec<&str> = kept.iter().map(|(_, why)| why.as_str()).collect();
                tx.execute(
                    "UPDATE build_items SET error = ?3 WHERE build_id = ?1 AND seq = ?2",
                    params![
                        p.build,
                        w.seq,
                        format!(
                            "{} is placed here, and kept in source set {} too: {}",
                            w.item.game,
                            p.set_name,
                            whys.join("; ")
                        )
                    ],
                )?;
            }
            set_place_state(tx, p.build, w.seq, Some("done"))
        })?;
        Ok(Ok(()))
    }

    /// Rolls forward or back each Game Build `build`, of plan `plan`, was placing when it stopped, once its Library
    /// Folder `folder` is cleaned up: through the folder, with its marker at the store's generation, and the Source
    /// Set's folder. `Err` says why one is left outstanding.
    pub(super) fn recover_placing(
        &self,
        config: &Config,
        build: i64,
        plan: i64,
        folder: &crate::folders::FolderRow,
        store: String,
    ) -> Result<std::result::Result<(), String>> {
        let open: bool = self.read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM build_items WHERE build_id = ?1
                                 AND place_state IN ('renaming', 'copying', 'converting', 'disposing'))",
                [build],
                |r| r.get(0),
            )?)
        })?;
        if !open {
            return Ok(Ok(()));
        }
        let dir = match crate::folders::open_writable_folder(config, &folder.root, &folder.path) {
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
        let items = self.plan(plan)?.content.items;
        self.recover_placements(config, build, &items, (&dir, &expected))
    }

    /// Rolls forward or back each placing item of Build `build`, of plan `items`, that a stop left part way, through
    /// the Library Folder `dir` and the Source Set's folder, opened now. `Err` says why one is left outstanding.
    fn recover_placements(
        &self,
        config: &Config,
        build: i64,
        items: &[PlanItem],
        (dir, expected): (&FolderDir, &Marker),
    ) -> Result<std::result::Result<(), String>> {
        let (folder, set, set_name, audit, open): (i64, i64, String, i64, Vec<Stopped>) =
            self.read(|c| {
                let (folder, set, name, audit) = c.query_row(
                    "SELECT b.folder_id, f.source_set_id, s.name, b.audit_id
                     FROM builds b JOIN library_folders f ON f.id = b.folder_id
                     JOIN source_sets s ON s.id = f.source_set_id WHERE b.id = ?1",
                    [build],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?;
                let open = c
                    .prepare(
                        "SELECT seq, target, place_state, state FROM build_items
                         WHERE build_id = ?1 AND place_state IN ('renaming', 'copying', 'converting', 'disposing')
                         ORDER BY seq",
                    )?
                    .query_map([build], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                Ok((folder, set, name, audit, open))
            })?;
        if open.is_empty() {
            return Ok(Ok(()));
        }
        let set_dirs = match self.open_placing(config, set)? {
            Ok(dirs) => dirs,
            Err(why) => {
                return Ok(Err(format!(
                    "a game this build was placing is not finished, and source set {set_name}'s folder can't be \
                     opened to finish it: {why}"
                )));
            }
        };
        let tally = Tally::default();
        let p = Placing {
            build,
            folder,
            set,
            set_name: &set_name,
            audit,
            dir,
            expected,
            to: &tally,
            from: &tally,
        };
        for (seq, target, state, item_state) in open {
            let Some(item) = items
                .iter()
                .find(|i| i.target == target && i.place.is_some())
            else {
                return Ok(Err(format!(
                    "{target} was being placed, but its plan no longer names it"
                )));
            };
            let place = item.place.as_ref().expect("found with one");
            let w = Work { seq, item };
            let outcome = match state.as_str() {
                "renaming" => match place.originals.as_slice() {
                    [o] => self.finish_rename(&p, &set_dirs, &w, o, None)?,
                    _ => Ok(()),
                },
                "copying" => match place.originals.as_slice() {
                    [o] => self.recover_copy(&p, &set_dirs, &w, o)?,
                    _ => Ok(()),
                },
                // Its new file put in place and recorded built, by the Build or by recovery: it is placed. Otherwise
                // nothing of it is in place, and the Game stays in its Source Set.
                "converting" if item_state == "applied" => {
                    self.finish_convert(&p, &set_dirs, &w, false)?
                }
                "converting" => {
                    self.place_failed(&p, &w, "the build stopped before it was placed")?;
                    Ok(())
                }
                _ => self.dispose_originals(&p, &set_dirs, &w)?,
            };
            if let Err(why) = outcome {
                return Ok(Err(why));
            }
        }
        Ok(Ok(()))
    }

    /// Finishes the copy of `o` a stop left part way: once published, it is checked and recorded placed, then `o`
    /// deleted; before that, the copy is undone and `o` left.
    fn recover_copy(
        &self,
        p: &Placing,
        set: &SetDirs,
        w: &Work,
        o: &PlacedOriginal,
    ) -> Result<std::result::Result<(), String>> {
        let intents = CopyIntents {
            store: self,
            build: p.build,
            seq: w.seq,
        };
        let intent = intents.load()?;
        let published = match (intent.staged_id, &intent.sha256) {
            (Some(id), Some(_)) => p
                .dir
                .stat(&w.item.target)
                .map_err(io_err)?
                .is_some_and(|st| {
                    FolderDir::kind(&st) == FileType::RegularFile && Identity::of(&st) == id
                }),
            _ => false,
        };
        if published {
            return self.finish_copy(p, set, w, o);
        }
        // Never published: a copy staged is removed while it is the one recorded; anything else there is left.
        if let (Some(staged), Some(id)) = (&intent.staged, intent.staged_id)
            && let Some(st) = p.dir.stat(staged).map_err(io_err)?
            && FolderDir::kind(&st) == FileType::RegularFile
            && Identity::of(&st) == id
        {
            p.dir.unlink(staged).map_err(io_err)?;
            p.dir.sync_holding(staged).map_err(io_err)?;
        }
        intents.save(&Intent::default())?;
        self.place_failed(
            p,
            w,
            "the build stopped before it was copied into the folder",
        )?;
        Ok(Ok(()))
    }
}

/// Writes `item`, a Game placed by converting it, to `file`, new in the staging folder, from its sources in the
/// Source Set's folder `set`; then verifies it.
fn stage_placed(
    item: &PlanItem,
    file: &mut File,
    set: &FolderDir,
    tallies: (&Tally, &Tally),
    built_at: SystemTime,
) -> std::result::Result<Staged, Failure> {
    let set = set.try_clone().map_err(|e| e.to_string());
    stage_by_fd(
        item,
        file,
        &|source| open_source(&set, source),
        tallies,
        built_at,
    )
}
