//! Placing Games (ADR 0023): the Build items of kind `place` of one release make its files in the Library Folder
//! holding its Source Set's Placed Games their Games' only copy, and so change the Source Set. A release is placed
//! whole, every file of it the one way, or not at all.
//!
//! - **Rename**: each Source Set file is the file the folder stores, on the same filesystem. Each is held open,
//!   checked to be the file the plan found by its device, inode, size, modification time and change time, and renamed
//!   into place by a rename that never replaces anything. Each is checked again through the descriptor held across the
//!   rename, and its new change time read through it is recorded: it is never read, and its Audit's Entries and the
//!   payload read an Incremental Audit reuses follow it, their Evidence carried over.
//! - **Copy**: the same files on another filesystem, or where a rename can't reach. Each is copied under a staged
//!   name, read back against what the original read as while it held still, and published by a rename that never
//!   replaces anything ([`crate::carry::copy`]).
//! - **Convert**: the folder stores the Games in another format. Each Source Set file they are written from is read
//!   whole while it holds still and recorded first; the new files are written and checked in the staging folder as
//!   any built file is, and published together, as one unit, the same way.
//! - **Built**: the folder holds the release's files already, as built. The Source Set's files are read and recorded
//!   as for a conversion, and each built file is read again to check it still holds what it was built as.
//!
//! Before anything moves, each folder a file goes in is made, and the identity of every folder from the Library Folder
//! down to it recorded. Every file is checked to be in place, in those folders, walked again without following a
//! symbolic link from the Library Folder's registered path with its marker, before the release is recorded placed and
//! again as the last step before each Source Set file is consumed.
//!
//! The release's Games are recorded placed, their files recorded built, and their Source Set's Audit made to name
//! them there, in the one transaction that ends the step that put them in place, and only for the files found there.
//! Only then is each Source Set file they consumed deleted, under ADR 0010's trusted-stamp rule
//! ([`crate::carry::dispose_as`]): set aside into the Source Set's private `.deleting` folder through a descriptor
//! held across that rename, read back, and deleted only while it still reads as recorded and the placed file, held
//! open, still reads as it was placed. One that can't be proven unchanged is moved into the Source Set's kept files,
//! recorded as kept by the Build. So at every step each Game has a recorded copy in one place: its Source Set's files
//! until it is placed, then its placed files. Each step is recorded before it is taken, and recovery rolls it forward
//! or back; one it can't finish is left outstanding, with why, for the next start.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io;
use std::time::SystemTime;

use rusqlite::{OptionalExtension, Transaction, params};
use rustix::fs::{FileType, Stat};
use title_select_api::build::{
    FileIdentity, ItemState, Marker, PlaceHow, PlacedOriginal, PlanItem, PlanPlace, kept_name,
};
use title_select_api::lifecycle::RegistrationKind;
use title_select_api::storage::{placed_path, release_of};
use title_select_engine::meter::Tally;

use super::{
    ChdWork, Failure, Scratch, Staged, Work, chd_dir, dat_cue, is_file, make_parents, open_source,
    record_applied, record_layout_dir, record_temp, remove_staged, set_item, stage_by_fd,
    stage_chd, staged_temp, still_sha256,
};
use crate::carry::{self, Intent, Intents};
use crate::folder_dir::{FolderDir, Identity, ctime_ns, file_stat, parent_made, parent_of};
use crate::folders::{check_marker, open_writable_folder};
use crate::ingest::moves::descend;
use crate::kept::{Scope, open_area_deleting, open_retention, settle_dir_temps};
use crate::placements::Rewritten;
use crate::repairs::{MarkerRule, open_repairable, source_folder};
use crate::{Error, Fault, Result, Store, now};

fn io_err(e: impl std::fmt::Display) -> Error {
    Error::Io(e.to_string())
}

/// The Source Set's folder a placing Build changes, opened to write to, and its private `.deleting` folder.
pub(super) struct SetDirs {
    pub dir: FolderDir,
    pub deleting: FolderDir,
}

/// What placing a release needs of its Build.
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
    /// Where the Library Folder is registered: walked again from there to check a placed file is in place.
    pub roots: &'a crate::roots::Roots,
    pub root: &'a str,
    pub path: &'a str,
    /// The Source Set folder's generation, which a Source Set file kept instead of deleted is kept under.
    pub generation: u64,
    /// Where what is read and written in the Library Folder's Root is counted, and in the Source Set's.
    pub to: &'a Tally,
    pub from: &'a Tally,
}

/// A file of a release recorded placed, as its item records it: seq, target, game, device, inode and SHA-256.
type PlacedRow = (
    i64,
    String,
    String,
    Option<i64>,
    Option<i64>,
    Option<String>,
);

/// A placing item a stop left part way: its seq, target, placement state and item state.
type Stopped = (i64, String, String, String);

/// A Source Set file a placing item recorded before it changed anything: as the plan found it, what it read as, and
/// where it was set aside to delete, or is being kept, with why.
struct OriginalRow {
    n: i64,
    original: PlacedOriginal,
    sha256: Option<String>,
    slot: Option<String>,
    kept: Option<(String, u64)>,
    detail: Option<String>,
}

fn original_rows(c: &rusqlite::Connection, build: i64, seq: i64) -> Result<Vec<OriginalRow>> {
    let mut stmt = c.prepare(
        "SELECT n, path, size, mtime_ns, dev, ino, ctime_ns, sha256, slot, kept_name, kept_generation, detail
         FROM placement_originals WHERE build_id = ?1 AND seq = ?2 AND state = 'pending' ORDER BY n",
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
            slot: r.get(8)?,
            kept: match (r.get::<_, Option<String>>(9)?, r.get::<_, Option<i64>>(10)?) {
                (Some(name), Some(generation)) => Some((name, generation as u64)),
                _ => None,
            },
            detail: r.get(11)?,
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

fn set_original_state(
    tx: &Transaction,
    build: i64,
    (seq, n): (i64, i64),
    state: &str,
) -> Result<()> {
    tx.execute(
        "UPDATE placement_originals SET state = ?4 WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
        params![build, seq, n, state],
    )?;
    Ok(())
}

/// The placement of `w`, an item of kind `place`.
fn placement<'a>(w: &Work<'a>) -> Result<&'a PlanPlace> {
    w.item
        .place
        .as_ref()
        .ok_or_else(|| Error::Io(format!("{} has no placement", w.item.target)))
}

/// The Source Set files `unit` is written from, each once, and each file it puts at `paths`, as Audits name them,
/// whose Entries [`crate::placements::rewritten_entries`] writes for a release not placed by a rename or a copy.
fn rewrites<'a>(
    unit: &[&Work<'a>],
    paths: &'a [String],
) -> Result<(Vec<&'a str>, Vec<Rewritten<'a>>)> {
    let mut from: Vec<&str> = Vec::new();
    for w in unit {
        for o in &placement(w)?.originals {
            if !from.contains(&o.path.as_str()) {
                from.push(&o.path);
            }
        }
    }
    let to = unit
        .iter()
        .zip(paths)
        .map(|(w, path)| Rewritten {
            path,
            game: &w.item.game,
            roms: &w.item.roms,
            sources: &w.item.sources,
            loose: w.item.loose,
        })
        .collect();
    Ok((from, to))
}

/// Where a placing item's file goes in the Library Folder: the folder it goes in, opened from the Library Folder
/// without following a symbolic link, its name there, and the identity of each folder from the Library Folder down to
/// it.
struct Spot {
    dir: FolderDir,
    leaf: String,
    chain: Vec<Identity>,
}

impl Spot {
    fn dest<'a>(&'a self, p: &'a Placing) -> carry::Dest<'a> {
        carry::Dest {
            root: p.dir,
            dir: &self.dir,
            name: &self.leaf,
            tally: p.to,
        }
    }
}

fn spot(dir: &FolderDir, target: &str) -> io::Result<Spot> {
    let (at, chain) = descend(dir.try_clone()?, target)?;
    Ok(Spot {
        dir: at,
        leaf: target.rsplit('/').next().unwrap_or(target).to_owned(),
        chain,
    })
}

fn save_chain(tx: &Transaction, build: i64, seq: i64, chain: &[Identity]) -> Result<()> {
    let chain: Vec<(i64, i64)> = chain.iter().map(|id| id.to_sql()).collect();
    tx.execute(
        "UPDATE build_items SET place_chain = ?3 WHERE build_id = ?1 AND seq = ?2",
        params![build, seq, serde_json::to_string(&chain).map_err(io_err)?],
    )?;
    Ok(())
}

fn chain_of(c: &rusqlite::Connection, build: i64, seq: i64) -> Result<Option<Vec<Identity>>> {
    let json: Option<String> = c.query_row(
        "SELECT place_chain FROM build_items WHERE build_id = ?1 AND seq = ?2",
        params![build, seq],
        |r| r.get(0),
    )?;
    let Some(json) = json else {
        return Ok(None);
    };
    let chain: Vec<(i64, i64)> = serde_json::from_str(&json).map_err(io_err)?;
    Ok(Some(
        chain
            .into_iter()
            .map(|(dev, ino)| Identity::from_sql(dev, ino))
            .collect(),
    ))
}

/// Whether `target` is still below `chain`, the folders it was put in, in the Library Folder at its registered path,
/// with its marker: each walked again from there without following a symbolic link. Returns why not.
fn in_place(p: &Placing, chain: &[Identity], target: &str) -> std::result::Result<(), String> {
    let dir = open_writable_folder(p.roots, p.root, p.path).map_err(|why| {
        format!("the play library folder can't be opened where it is registered: {why}")
    })?;
    check_marker(&dir, p.expected)?;
    match descend(dir, target) {
        Ok((_, now)) if now == chain => Ok(()),
        Ok(_) => Err(format!(
            "the folder {target} was put in is no longer the one at the play library folder's path"
        )),
        Err(e) => Err(format!(
            "the folder {target} was put in can't be opened from the play library folder's path ({e})"
        )),
    }
}

/// Where Copy or Convert records the copy of a placing item's one Source Set file's [`Intent`]: on the item's row.
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

/// A file of a release recorded placed, which the release's Source Set files are deleted only while it is in place
/// too: one ZIP converted to several loose files stands in for all of them. Where it is, held open from before any of
/// them is set aside to delete; what it was placed as; or why it can't be found as it was placed.
struct Output {
    seq: i64,
    target: String,
    sha: Option<String>,
    found: std::result::Result<(Spot, carry::Held), String>,
}

impl Output {
    /// Whether it is still in place, as it was placed: the file held, reading as recorded, below the folders recorded
    /// walked again from the Library Folder's registered path. Returns why not.
    fn holds(&self, p: &Placing) -> Result<std::result::Result<(), String>> {
        let (s, held) = match &self.found {
            Ok(found) => found,
            Err(why) => return Ok(Err(why.clone())),
        };
        if let Some(sha) = &self.sha
            && let Err(why) = held.still(&s.dir, &s.leaf, sha, p.to)?
        {
            return Ok(Err(format!("{}: {why}", self.target)));
        }
        Ok(in_place(p, &s.chain, &self.target))
    }
}

/// Where the deletion of a placing item's Source Set file `n` records its [`Intent`]: its slot in the private
/// `.deleting` folder on its own row; the placed file it stands in for, `published`, and what the original read as,
/// are recorded already. It is deleted only while the placed file is in place, below `chain`; otherwise, or when it
/// can't be proven unchanged, it is kept.
struct DisposeIntents<'a> {
    store: &'a Store,
    p: &'a Placing<'a>,
    set: &'a SetDirs,
    seq: i64,
    n: i64,
    target: &'a str,
    chain: &'a [Identity],
    published: Identity,
    /// Every other file of the release recorded placed.
    outputs: &'a [Output],
}

impl Intents for DisposeIntents<'_> {
    fn load(&self) -> Result<Intent> {
        self.store.read(|c| {
            Ok(c.query_row(
                "SELECT sha256, slot, moved_ctime_ns FROM placement_originals
                 WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                params![self.p.build, self.seq, self.n],
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
                    self.p.build,
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

    fn in_place(&self) -> Result<std::result::Result<(), String>> {
        if let Err(why) = in_place(self.p, self.chain, self.target) {
            return Ok(Err(why));
        }
        // Then every other file of the release, each checked last as this one is: a Source Set file is consumed only
        // once every file that may stand in for it is in place.
        for output in self.outputs.iter().filter(|o| o.seq != self.seq) {
            if let Err(why) = output.holds(self.p)? {
                return Ok(Err(why));
            }
        }
        Ok(Ok(()))
    }

    fn keep_instead(
        &self,
        o: &carry::Original,
        slot: &str,
        why: &str,
    ) -> Result<Option<std::result::Result<String, String>>> {
        Ok(Some(self.store.keep_placed_original(
            self.p,
            self.set,
            (self.seq, self.n, o.label),
            (o.deleting, slot),
            None,
            why,
        )?))
    }
}

/// How a placed file is recorded built, in the transaction that records it placed.
#[derive(Clone, Copy)]
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

/// A file of a release, in place: found at its target as `at`, statted through a descriptor as `now`, and read as
/// `sha` if it was read.
struct Found {
    at: Stat,
    now: Stat,
    sha: Option<String>,
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

/// The one Source Set file a renamed or copied item moves.
fn one<'a>(w: &Work<'a>) -> Result<Option<&'a PlacedOriginal>> {
    Ok(match placement(w)?.originals.as_slice() {
        [o] => Some(o),
        _ => None,
    })
}

fn is_regular(st: &Stat, id: Identity) -> bool {
    FolderDir::kind(st) == FileType::RegularFile && Identity::of(st) == id
}

impl Store {
    /// Opens Source Set `set`'s folder for a Build to place its Games from, as a repair opens it: only while it is
    /// registered, its Root lets Title Select write there, and its marker is this store's, written first if no repair
    /// wrote one. Returns why not.
    pub(super) fn open_placing(
        &self,
        set: i64,
    ) -> Result<std::result::Result<SetDirs, String>> {
        let roots = &*self.roots();
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
        let dir = match open_repairable(self, roots, set, rule)? {
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

    /// Places the release of `unit`, its items of kind `place`, as its plan says: every file of it, or none. Each
    /// failure that leaves the release as it was, in its Source Set, fails only its items; one that leaves anything
    /// to finish stops the Build, for recovery.
    pub(super) fn place_unit(
        &self,
        p: &Placing,
        set: &SetDirs,
        unit: &[&Work],
        (staging, staged_ids, built_at): (&FolderDir, &mut HashMap<String, Identity>, SystemTime),
        chd: &ChdWork,
    ) -> Result<()> {
        check_marker(p.dir, p.expected).map_err(Error::Refused)?;
        let how = placement(unit[0])?.how;
        for w in unit {
            if placement(w)?.how != how {
                return self.fail_unit(p, unit, "its files are not all placed the same way");
            }
        }
        // Each folder a file goes in is made, and every folder down to it recorded, before anything moves.
        let mut spots = Vec::new();
        for w in unit {
            let target = &w.item.target;
            let found = make_parents(p.dir, target)
                .map_err(|e| e.to_string())
                .and_then(|made| {
                    record_layout_dir(self, p.folder, made).map_err(|e| e.to_string())?;
                    spot(p.dir, target).map_err(|e| e.to_string())
                });
            match found {
                Ok(found) => spots.push(found),
                Err(e) => {
                    return self.fail_unit(
                        p,
                        unit,
                        &format!("the folder {target} goes in could not be opened: {e}"),
                    );
                }
            }
        }
        self.write(|tx| {
            for (w, s) in unit.iter().zip(&spots) {
                save_chain(tx, p.build, w.seq, &s.chain)?;
            }
            Ok(())
        })?;
        match how {
            PlaceHow::Rename => self.place_rename(p, set, unit, &spots),
            PlaceHow::Copy => self.place_copy(p, set, unit, &spots),
            PlaceHow::Convert => {
                self.place_convert(p, set, unit, &spots, (staging, staged_ids, built_at), chd)
            }
            PlaceHow::Built => self.place_built(p, set, unit, &spots, chd),
        }
    }

    /// Whether the Audit has, for each ROM `unit` writes, the Entry it is written from, with its Evidence, to carry
    /// over to the file placed; or why not. Checked before anything of a conversion or a built file is placed, so that
    /// recording it placed can't fail once it is in place.
    fn carries(&self, p: &Placing, unit: &[&Work]) -> Result<std::result::Result<(), String>> {
        let paths: Vec<String> = unit
            .iter()
            .map(|w| placed_path(p.folder, &w.item.target))
            .collect();
        let (from, to) = rewrites(unit, &paths)?;
        Ok(self
            .read(|c| crate::placements::rewritten(c, p.audit, &from, &to))?
            .map(|_| ()))
    }

    /// Fails every item of `unit` with `why`, its release left in its Source Set as it was.
    pub(super) fn fail_unit(&self, p: &Placing, unit: &[&Work], why: &str) -> Result<()> {
        self.write(|tx| {
            for w in unit {
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
                )?;
            }
            Ok(())
        })
    }

    fn place_rename(
        &self,
        p: &Placing,
        set: &SetDirs,
        unit: &[&Work],
        spots: &[Spot],
    ) -> Result<()> {
        let mut held = Vec::new();
        for w in unit {
            let Some(o) = one(w)? else {
                return self.fail_unit(p, unit, "a rename moves one file for each of its files");
            };
            match hold(&set.dir, o)? {
                Ok(file) => held.push(file),
                Err(why) => return self.fail_unit(p, unit, &why),
            }
        }
        self.write(|tx| {
            for w in unit {
                set_place_state(tx, p.build, w.seq, Some("renaming"))?;
            }
            Ok(())
        })?;
        let mut moved: Vec<(&Work, &Spot)> = Vec::new();
        for (w, s) in unit.iter().zip(spots) {
            let o = one(w)?.expect("checked above");
            self.fault_point(Fault::PlaceRenaming(w.seq));
            let renamed = parent_of(&set.dir, &o.path).and_then(|(parent, name)| {
                parent
                    .as_ref()
                    .unwrap_or(&set.dir)
                    .rename_new(name, &s.dir, &s.leaf)
            });
            if let Err(e) = renamed {
                if moved.is_empty() && self.across_filesystems(Some(&e)) {
                    // No rename reaches the folder from here: the release is copied instead.
                    drop(held);
                    self.write(|tx| {
                        for w in unit {
                            set_place_state(tx, p.build, w.seq, None)?;
                        }
                        Ok(())
                    })?;
                    return self.place_copy(p, set, unit, spots);
                }
                let why = format!("{} could not be moved into the folder: {e}", o.path);
                return self.unmove(p, set, unit, &moved, &why);
            }
            moved.push((w, s));
            self.fault_point(Fault::PlaceMoved(w.seq));
        }
        // Every file moved: each checked, through the descriptor held across its rename, to be the file the plan
        // found, and to be in place, below the folders recorded; then all recorded placed together, or all put back.
        let mut found = Vec::new();
        for ((w, s), file) in moved.iter().zip(&held) {
            let o = one(w)?.expect("checked above");
            sync_home(set, o)?;
            s.dir.sync().map_err(io_err)?;
            let id = Identity::from(o.identity);
            let at = s.dir.stat(&s.leaf).map_err(io_err)?;
            let now = rustix::fs::fstat(file).map_err(io_err)?;
            let why = match &at {
                Some(at)
                    if is_regular(at, id)
                        && Identity::of(&now) == id
                        && file_stat(&now) == o.file =>
                {
                    in_place(p, &s.chain, &w.item.target).err()
                }
                Some(at) if is_regular(at, id) => {
                    Some(format!("{} changed as it was moved", o.path))
                }
                _ => Some(format!("{} was replaced as it was moved", o.path)),
            };
            if let Some(why) = why {
                return self.unmove(p, set, unit, &moved, &why);
            }
            found.push(Found {
                at: at.expect("checked above"),
                now,
                sha: None,
            });
        }
        self.record_unit(p, unit, &found, Built::Found, "done", None)
    }

    /// Puts each file of `moved`, items of `unit` renamed into the folder by this Build, back where it was, then fails
    /// `unit` with `why`. Whatever was renamed goes back, whether it is the file the plan found or one that took its
    /// name first. One that can't be put back stops the Build, leaving `unit` for recovery.
    fn unmove(
        &self,
        p: &Placing,
        set: &SetDirs,
        unit: &[&Work],
        moved: &[(&Work, &Spot)],
        why: &str,
    ) -> Result<()> {
        for (w, s) in moved {
            let o = one(w)?.expect("a renamed item");
            if let Err(e) = move_home(set, o, s) {
                return Err(Error::Refused(format!(
                    "{why}, and {} could not be put back ({e}); it is at {} until it can be",
                    o.path,
                    s.dir.path().join(&s.leaf).display()
                )));
            }
        }
        let why = match moved.is_empty() {
            true => why.to_owned(),
            false => format!("{why}, so its files were put back"),
        };
        self.fail_unit(p, unit, &why)
    }

    /// Finishes the renames of `unit` a stop left part way. Once every file is at its target, below the folders
    /// recorded, the release is recorded placed; otherwise each one moved goes back. `Err` says why it is left
    /// outstanding: a file neither where it was nor in place.
    fn recover_rename(
        &self,
        p: &Placing,
        set: &SetDirs,
        unit: &[&Work],
    ) -> Result<std::result::Result<(), String>> {
        let mut moved: Vec<(&Work, Spot, Stat)> = Vec::new();
        let mut home = 0;
        for w in unit {
            let Some(o) = one(w)? else {
                return Ok(Err(format!(
                    "{} was being moved, but its plan moves no one file",
                    w.item.target
                )));
            };
            let id = Identity::from(o.identity);
            let at_home = match parent_of(&set.dir, &o.path) {
                Ok((parent, name)) => parent
                    .as_ref()
                    .unwrap_or(&set.dir)
                    .stat(name)
                    .map_err(io_err)?
                    .is_some_and(|st| is_regular(&st, id)),
                Err(_) => false,
            };
            if at_home {
                home += 1;
                continue;
            }
            let at_target = match self.recorded_spot(p, w)? {
                Ok(s) => match s.dir.stat(&s.leaf).map_err(io_err)? {
                    Some(st) if is_regular(&st, id) => Ok((s, st)),
                    _ => Err(format!("it is not at {}", w.item.target)),
                },
                Err(why) => Err(why),
            };
            match at_target {
                Ok((s, st)) => moved.push((w, s, st)),
                // Neither where it was nor where it was going: nothing records a copy of the game, so the placement is
                // left outstanding rather than ended.
                Err(why) => {
                    return Ok(Err(format!(
                        "{} is no longer in source set {}, and {why}; it is left as it is until it is back in the \
                         source set or in place, and the build finishes then",
                        o.path, p.set_name
                    )));
                }
            }
        }
        if home == 0 {
            let mut found = Vec::new();
            let mut changed = None;
            for (w, s, st) in &moved {
                let o = one(w)?.expect("checked above");
                let file = s.dir.open_file(&s.leaf).map_err(io_err)?;
                let now = rustix::fs::fstat(&file).map_err(io_err)?;
                if Identity::of(&now) != Identity::of(st) || file_stat(&now) != o.file {
                    changed = Some(format!("{} changed as it was moved", o.path));
                    break;
                }
                found.push(Found {
                    at: *st,
                    now,
                    sha: None,
                });
            }
            if changed.is_none() {
                self.record_unit(p, unit, &found, Built::Found, "done", None)?;
                return Ok(Ok(()));
            }
        }
        for (w, s, _) in &moved {
            let o = one(w)?.expect("checked above");
            if let Err(e) = move_home(set, o, s) {
                return Ok(Err(format!(
                    "the build stopped before every file of {} was moved, and {} could not be put back ({e}); it is \
                     at {} until it can be",
                    w.item.game,
                    o.path,
                    s.dir.path().join(&s.leaf).display()
                )));
            }
        }
        self.fail_unit(
            p,
            unit,
            "the build stopped before its files were all moved and checked, so those moved were put back",
        )?;
        Ok(Ok(()))
    }

    /// Where `w`'s file goes, in the Library Folder opened now, while every folder down to it is the one recorded
    /// before anything of its release moved. Returns why not.
    fn recorded_spot(&self, p: &Placing, w: &Work) -> Result<std::result::Result<Spot, String>> {
        self.recorded_spot_of(p, w.seq, &w.item.target)
    }

    /// Where item `seq`'s file `target` goes, as [`Store::recorded_spot`].
    fn recorded_spot_of(
        &self,
        p: &Placing,
        seq: i64,
        target: &str,
    ) -> Result<std::result::Result<Spot, String>> {
        let Some(chain) = self.read(|c| chain_of(c, p.build, seq))? else {
            return Ok(Err(format!("where {target} was put isn't recorded")));
        };
        Ok(match spot(p.dir, target) {
            Ok(s) if s.chain == chain => Ok(s),
            Ok(_) => Err(format!(
                "the folder {target} was put in is no longer the one at the play library folder's path"
            )),
            Err(e) => Err(format!(
                "the folder {target} was put in can't be opened ({e})"
            )),
        })
    }

    fn place_copy(&self, p: &Placing, set: &SetDirs, unit: &[&Work], spots: &[Spot]) -> Result<()> {
        for w in unit {
            if one(w)?.is_none() {
                return self.fail_unit(p, unit, "a copy is of one file for each of its files");
            }
        }
        self.write(|tx| {
            for w in unit {
                let o = one(w)?.expect("checked above");
                insert_original(tx, (p.build, w.seq, 0), o, None)?;
                set_place_state(tx, p.build, w.seq, Some("copying"))?;
            }
            Ok(())
        })?;
        match self.copy_unit(p, set, unit, spots)? {
            Ok(()) => Ok(()),
            Err(why) => Err(Error::Refused(why)),
        }
    }

    /// Copies each file of `unit` into the folder, or finishes the copies a stop left: each published and checked to
    /// hold what its original read as, in place below the folders recorded; then all recorded placed together, and
    /// only then each original deleted. A copy that fails undoes every copy of the release. `Err` says why it is left
    /// outstanding.
    fn copy_unit(
        &self,
        p: &Placing,
        set: &SetDirs,
        unit: &[&Work],
        spots: &[Spot],
    ) -> Result<std::result::Result<(), String>> {
        let mut published = Vec::new();
        for (w, s) in unit.iter().zip(spots) {
            let o = one(w)?.expect("a copied item");
            let (parent, name) = match parent_of(&set.dir, &o.path) {
                Ok(found) => found,
                Err(e) => {
                    let why = format!("{} can't be reached: {e}", o.path);
                    return self.discard_copies(p, unit, spots, &why);
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
            let intents = CopyIntents {
                store: self,
                build: p.build,
                seq: w.seq,
            };
            match carry::copy(&original, &s.dest(p), &intents)? {
                Ok(st) => published.push(st),
                Err(why) => return self.discard_copies(p, unit, spots, &why),
            }
            self.fault_point(Fault::PlaceCopied(w.seq));
        }
        // Each checked again before it stands in for its original, here or after a stop.
        let mut found = Vec::new();
        for ((w, s), st) in unit.iter().zip(spots).zip(published) {
            let o = one(w)?.expect("a copied item");
            let intents = CopyIntents {
                store: self,
                build: p.build,
                seq: w.seq,
            };
            let checked = match carry::published_holds(&s.dest(p), &intents)? {
                Ok(()) => in_place(p, &s.chain, &w.item.target),
                Err(why) => Err(why),
            };
            if let Err(why) = checked {
                let why = format!("{} was copied into the folder, but {why}", o.path);
                return self.discard_copies(p, unit, spots, &why);
            }
            found.push(Found {
                at: st,
                now: st,
                sha: intents.load()?.sha256,
            });
        }
        self.write(|tx| {
            for (w, f) in unit.iter().zip(&found) {
                tx.execute(
                    "UPDATE placement_originals SET sha256 = ?3 WHERE build_id = ?1 AND seq = ?2 AND n = 0",
                    params![p.build, w.seq, f.sha],
                )?;
            }
            Ok(())
        })?;
        self.record_unit(p, unit, &found, Built::Found, "disposing", None)?;
        self.dispose_unit(p, set, unit, spots)
    }

    /// Removes each copy of `unit` published or staged, while it is the copy recorded, then fails `unit` with `why`:
    /// nothing of its originals has changed. Anything else at a copy's name is left.
    fn discard_copies(
        &self,
        p: &Placing,
        unit: &[&Work],
        spots: &[Spot],
        why: &str,
    ) -> Result<std::result::Result<(), String>> {
        for (w, s) in unit.iter().zip(spots) {
            let intents = CopyIntents {
                store: self,
                build: p.build,
                seq: w.seq,
            };
            let intent = intents.load()?;
            if let Some(id) = intent.staged_id {
                if let Some(st) = s.dir.stat(&s.leaf).map_err(io_err)?
                    && is_regular(&st, id)
                {
                    s.dir.unlink(&s.leaf).map_err(io_err)?;
                    s.dir.sync().map_err(io_err)?;
                }
                if let Some(staged) = &intent.staged
                    && let Some(st) = p.dir.stat(staged).map_err(io_err)?
                    && is_regular(&st, id)
                {
                    p.dir.unlink(staged).map_err(io_err)?;
                    p.dir.sync_holding(staged).map_err(io_err)?;
                }
            }
            intents.save(&Intent::default())?;
        }
        self.fail_unit(p, unit, why)?;
        Ok(Ok(()))
    }

    /// Finishes the copies of `unit` a stop left part way: once every copy is published, in place below the folders
    /// recorded, they are checked and recorded placed, then the originals deleted; before that, every copy is undone.
    /// `Err` says why it is left outstanding.
    fn recover_copy(
        &self,
        p: &Placing,
        set: &SetDirs,
        unit: &[&Work],
    ) -> Result<std::result::Result<(), String>> {
        let mut spots = Vec::new();
        for w in unit {
            match self.recorded_spot(p, w)? {
                Ok(s) => spots.push(s),
                Err(why) => {
                    return Ok(Err(format!(
                        "{} was being copied into the folder, but {why}; it is left as it is until the folder is \
                         back, and the build finishes then",
                        w.item.target
                    )));
                }
            }
        }
        let mut all = true;
        for (w, s) in unit.iter().zip(&spots) {
            let intent = CopyIntents {
                store: self,
                build: p.build,
                seq: w.seq,
            }
            .load()?;
            all &= match (intent.staged_id, &intent.sha256) {
                (Some(id), Some(_)) => s
                    .dir
                    .stat(&s.leaf)
                    .map_err(io_err)?
                    .is_some_and(|st| is_regular(&st, id)),
                _ => false,
            };
        }
        match all {
            true => self.copy_unit(p, set, unit, &spots),
            false => self.discard_copies(
                p,
                unit,
                &spots,
                "the build stopped before every file of it was copied into the folder",
            ),
        }
    }

    /// Reads each Source Set file `w` is placed from whole, while it holds still as the plan found it, and records
    /// what it read as before anything is written for it. Returns why not.
    fn record_originals(
        &self,
        p: &Placing,
        set: &SetDirs,
        w: &Work,
    ) -> Result<std::result::Result<(), String>> {
        let place = placement(w)?;
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

    /// Keeps the cue sheet of `w`, a CD built in the folder as CHD already, in the store, and records it on the item:
    /// read from its source in the Source Set, the file the plan found there, and checked against the DAT. Returns why
    /// not. A DVD has none.
    fn keep_built_cue(
        &self,
        p: &Placing,
        set: &SetDirs,
        w: &Work,
        chd: &ChdWork,
    ) -> Result<std::result::Result<(), String>> {
        let Some(rom) = cue_rom(w.item) else {
            return Ok(Ok(()));
        };
        let Some(source) = w.item.sources.iter().find(|s| s.rom == rom.name) else {
            return Ok(Err(format!("no source was found for {}", rom.name)));
        };
        let opened = match open_source(&set.dir.try_clone().map_err(|e| e.to_string()), source) {
            Ok(opened) => opened,
            Err(why) => return Ok(Err(why)),
        };
        let dir = chd_dir(&chd.tmp, p.build, w.seq);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(io_err)?;
        let _scratch = Scratch(dir.clone());
        let bytes = match dat_cue(
            rom,
            source,
            &opened,
            &dir.join("dat.cue"),
            (p.from, &chd.scratch),
        ) {
            Ok(bytes) => bytes,
            Err(why) => return Ok(Err(format!("its cue sheet in the source set: {why}"))),
        };
        let sha = match self.keep_cue(&bytes) {
            Ok(sha) => sha,
            Err(e) => {
                return Ok(Err(format!(
                    "its cue sheet could not be kept in the store: {e}"
                )));
            }
        };
        self.fault_point(Fault::PlaceCueKept(w.seq));
        // Recorded only while it is still kept, within the write a collection of unheld blobs waits for.
        self.write(|tx| {
            if let Err(e) = self.kept_cue(&sha) {
                return Ok(Err(format!(
                    "its cue sheet was no longer kept in the store when it was to be recorded: {e}"
                )));
            }
            tx.execute(
                "UPDATE build_items SET cue_sha256 = ?3 WHERE build_id = ?1 AND seq = ?2",
                params![p.build, w.seq, sha],
            )?;
            Ok(Ok(()))
        })
    }

    fn place_convert(
        &self,
        p: &Placing,
        set: &SetDirs,
        unit: &[&Work],
        spots: &[Spot],
        (staging, staged_ids, built_at): (&FolderDir, &mut HashMap<String, Identity>, SystemTime),
        chd: &ChdWork,
    ) -> Result<()> {
        if let Err(why) = self.carries(p, unit)? {
            return self.fail_unit(p, unit, &why);
        }
        for w in unit {
            if let Err(why) = self.record_originals(p, set, w)? {
                return self.fail_unit(p, unit, &why);
            }
        }
        // Every file of the release written and verified before any is put in place: a CHD made by chdman, and
        // extracted again, every track it gives back matched against the DAT, as any CHD a Build makes.
        let mut ready: Vec<(String, File, Identity, Staged)> = Vec::new();
        let set_fd = set.dir.try_clone().map_err(|e| e.to_string());
        let discard = |ready: Vec<(String, File, Identity, Staged)>| -> Result<()> {
            for (name, file, id, _) in ready {
                drop(file);
                remove_staged(staging, &name, id).map_err(Error::Refused)?;
            }
            Ok(())
        };
        for w in unit {
            let name = format!("{}.zip", w.seq);
            let mut file = staging.create_new(&name).map_err(io_err)?;
            let file_id = Identity::of_fd(&file).map_err(io_err)?;
            record_temp(self, p.folder, &staged_temp(&name), file_id)?;
            staged_ids.insert(name.clone(), file_id);
            let staged = match w.item.chd {
                true => {
                    let at = staging.path().join(&name);
                    stage_chd(w.item, w.seq, &mut file, &at, &set_fd, chd, built_at)
                }
                false => stage_placed(w.item, &mut file, &set.dir, (p.from, p.to), built_at),
            };
            let why = match staged {
                Ok(staged) => {
                    ready.push((name, file, file_id, staged));
                    self.fault_point(Fault::Staged(w.seq));
                    continue;
                }
                Err(Failure::Folder(why)) => return Err(Error::Io(why)),
                Err(Failure::Item(why)) => why,
                Err(Failure::Cancelled) => "you cancelled the build".to_owned(),
            };
            drop(file);
            remove_staged(staging, &name, file_id).map_err(Error::Refused)?;
            discard(ready)?;
            return self.fail_unit(p, unit, &why);
        }
        // A CD's cue sheet, which its CHD doesn't hold, kept in the store, written and synced, before anything records
        // it: then recorded with the unit, and so held, from before the CHD can be put in place.
        let mut cues: Vec<Option<String>> = Vec::new();
        for (w, (_, _, _, staged)) in unit.iter().zip(&ready) {
            let Some(bytes) = &staged.cue else {
                cues.push(None);
                continue;
            };
            match self.keep_cue(bytes) {
                Ok(sha) => cues.push(Some(sha)),
                Err(e) => {
                    discard(ready)?;
                    let why = format!("its cue sheet could not be kept in the store: {e}");
                    return self.fail_unit(p, unit, &why);
                }
            }
            self.fault_point(Fault::PlaceCueKept(w.seq));
        }
        // Then committed: once one is put in place the rest follow, here or, after a stop, in recovery. Each cue sheet
        // is checked to be kept still within the commit, which a collection of unheld blobs waits for: from it on, the
        // unit holds them.
        let key = unit_key(&unit[0].item.game);
        let committed = self.write(|tx| {
            for sha in cues.iter().flatten() {
                if let Err(e) = self.kept_cue(sha) {
                    return Ok(Err(format!(
                        "its cue sheet was no longer kept in the store when it was to be recorded: {e}"
                    )));
                }
            }
            tx.execute(
                "INSERT INTO build_units (build_id, unit, state) VALUES (?1, ?2, 'committing')",
                params![p.build, key],
            )?;
            for ((w, (_, _, id, staged)), cue) in unit.iter().zip(&ready).zip(&cues) {
                let (dev, ino) = id.to_sql();
                tx.execute(
                    "UPDATE build_items SET unit = ?3, state = 'publishing', sha256 = ?4, size = ?5, dev = ?6,
                                            ino = ?7, mtime_ns = ?8, chd_sha1 = ?9, chd_raw_sha1 = ?10,
                                            cue_sha256 = ?11
                     WHERE build_id = ?1 AND seq = ?2",
                    params![
                        p.build,
                        w.seq,
                        key,
                        staged.sha256,
                        staged.size as i64,
                        dev,
                        ino,
                        staged.mtime_ns as i64,
                        staged.chd_sha1,
                        staged.chd_raw_sha1,
                        cue
                    ],
                )?;
            }
            Ok(Ok(()))
        })?;
        if let Err(why) = committed {
            discard(ready)?;
            return self.fail_unit(p, unit, &why);
        }
        for ((w, s), (name, _, id, _)) in unit.iter().zip(spots).zip(&ready) {
            self.fault_point(Fault::BeforePublish(w.seq));
            let target = &w.item.target;
            let renamed = check_marker(p.dir, p.expected)
                .and_then(|()| match is_file(staging, name, *id)? {
                    true => Ok(()),
                    false => Err(format!(
                        "{} was replaced after it was verified; it was left as it is",
                        staging.path().join(name).display()
                    )),
                })
                .and_then(|()| {
                    staging
                        .rename_new(name, &s.dir, &s.leaf)
                        .map_err(|e| match e.kind() {
                            io::ErrorKind::AlreadyExists => format!(
                                "{target} appeared in the folder during the build; it was left as it is"
                            ),
                            _ => e.to_string(),
                        })
                });
            if let Err(why) = renamed {
                return Err(Error::Refused(format!(
                    "the files of {} go in place together, but {target} could not be put in place: {why}. The \
                     build finishes it when the service next starts, once {target} can be put in place",
                    w.item.game
                )));
            }
            self.fault_point(Fault::PublishedUnsynced(w.seq));
            staging.sync().map_err(io_err)?;
            s.dir.sync().map_err(io_err)?;
            self.fault_point(Fault::AfterPublish(w.seq));
        }
        drop(ready);
        match self.finish_convert(p, set, unit, spots, true)? {
            Ok(()) => Ok(()),
            Err(why) => Err(Error::Refused(why)),
        }
    }

    /// Records `unit`'s converted files, published at their targets in `spots`, placed, once each is the file the
    /// Build verified and in place below the folders recorded, then deletes the Source Set files they were converted
    /// from; now or after a stop, when `record` says they are not yet recorded built. `Err` says why it is left
    /// outstanding.
    fn finish_convert(
        &self,
        p: &Placing,
        set: &SetDirs,
        unit: &[&Work],
        spots: &[Spot],
        record: bool,
    ) -> Result<std::result::Result<(), String>> {
        let mut found = Vec::new();
        let mut wrong = None;
        for (w, s) in unit.iter().zip(spots) {
            let (dev, ino, sha, cue): (Option<i64>, Option<i64>, Option<String>, Option<String>) =
                self.read(|c| {
                    Ok(c.query_row(
                        "SELECT dev, ino, sha256, cue_sha256 FROM build_items WHERE build_id = ?1 AND seq = ?2",
                        params![p.build, w.seq],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )?)
                })?;
            let target = &w.item.target;
            let (Some(dev), Some(ino), Some(sha)) = (dev, ino, sha) else {
                wrong = Some(format!("nothing written for {target} is recorded"));
                break;
            };
            // A CD placed as CHD is placed only with its cue sheet kept.
            if let Err(why) = cue_kept(self, w.item, cue.as_deref()) {
                wrong = Some(why);
                break;
            }
            match s.dir.stat(&s.leaf).map_err(io_err)? {
                Some(st) if is_regular(&st, Identity::from_sql(dev, ino)) => {
                    if let Err(why) = in_place(p, &s.chain, target) {
                        wrong = Some(why);
                        break;
                    }
                    found.push(Found {
                        at: st,
                        now: st,
                        sha: Some(sha),
                    });
                }
                _ => {
                    wrong = Some(format!("{target} is not the file the build put there"));
                    break;
                }
            }
        }
        if wrong.is_none()
            && let Err(why) = self.carries(p, unit)?
        {
            wrong = Some(why);
        }
        if let Some(why) = wrong {
            // Nothing of the Source Set's has changed: the release stays there, and what is in the folder is left.
            let key = unit_key(&unit[0].item.game);
            self.write(|tx| {
                tx.execute(
                    "UPDATE build_units SET state = 'done' WHERE build_id = ?1 AND unit = ?2",
                    params![p.build, key],
                )?;
                for w in unit {
                    set_place_state(tx, p.build, w.seq, None)?;
                    set_item(
                        tx,
                        p.build,
                        w.seq,
                        ItemState::Uncertain,
                        &format!(
                            "{why}; what is in the folder was left as it is, and {} stays in source set {}",
                            w.item.game, p.set_name
                        ),
                    )?;
                }
                Ok(())
            })?;
            return Ok(Ok(()));
        }
        let built = match record {
            true => Built::Published,
            false => Built::Recorded,
        };
        let key = unit_key(&unit[0].item.game);
        self.record_unit(p, unit, &found, built, "disposing", Some(&key))?;
        self.dispose_unit(p, set, unit, spots)
    }

    fn place_built(
        &self,
        p: &Placing,
        set: &SetDirs,
        unit: &[&Work],
        spots: &[Spot],
        chd: &ChdWork,
    ) -> Result<()> {
        if let Err(why) = self.carries(p, unit)? {
            return self.fail_unit(p, unit, &why);
        }
        let mut found = Vec::new();
        for (w, s) in unit.iter().zip(spots) {
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
                return self.fail_unit(
                    p,
                    unit,
                    &format!("{target} has no record of what it was built as"),
                );
            };
            let st = match s.dir.stat(&s.leaf).map_err(io_err)? {
                Some(st)
                    if FolderDir::kind(&st) == FileType::RegularFile
                        && file_stat(&st).size == size as u64
                        && file_stat(&st).mtime_ns == mtime_ns as i128 =>
                {
                    st
                }
                _ => {
                    return self.fail_unit(
                        p,
                        unit,
                        &format!("{target} changed since it was built"),
                    );
                }
            };
            if let Err(why) = self.record_originals(p, set, w)? {
                return self.fail_unit(p, unit, &why);
            }
            // A CD built here as CHD, checked by extracting it when it was built, is placed with its cue sheet, read
            // from the Source Set and checked against the DAT, kept in the store first.
            if w.item.chd
                && let Err(why) = self.keep_built_cue(p, set, w, chd)?
            {
                return self.fail_unit(p, unit, &why);
            }
            // Read again before it stands in for its Source Set's files: still the file built, as built, in place.
            let held = match carry::Held::open(&s.dir, &s.leaf, Identity::of(&st))? {
                Ok(held) => held,
                Err(why) => return self.fail_unit(p, unit, &format!("{target}: {why}")),
            };
            let checked = match held.still(&s.dir, &s.leaf, &sha, p.to)? {
                Ok(()) => in_place(p, &s.chain, target),
                Err(why) => Err(format!("{target}: {why}")),
            };
            if let Err(why) = checked {
                return self.fail_unit(p, unit, &why);
            }
            found.push(Found {
                at: st,
                now: st,
                sha: Some(sha),
            });
        }
        self.record_unit(p, unit, &found, Built::Earlier, "disposing", None)?;
        match self.dispose_unit(p, set, unit, spots)? {
            Ok(()) => Ok(()),
            Err(why) => Err(Error::Refused(why)),
        }
    }

    /// Records the release of `unit` placed as its files, `found` in place: each recorded built as `built` says, its
    /// Game placed, its Source Set's Audit made to name it there, and the item applied, with its placement at `next`;
    /// and its unit `unit` done, if it has one. All in one transaction.
    fn record_unit(
        &self,
        p: &Placing,
        unit: &[&Work],
        found: &[Found],
        built: Built,
        next: &str,
        key: Option<&str>,
    ) -> Result<()> {
        let paths: Vec<String> = unit
            .iter()
            .map(|w| placed_path(p.folder, &w.item.target))
            .collect();
        self.write(|tx| {
            for ((w, f), path) in unit.iter().zip(found).zip(&paths) {
                let item = w.item;
                let (dev, ino) = Identity::of(&f.at).to_sql();
                let file = file_stat(&f.at);
                if matches!(built, Built::Found | Built::Earlier) {
                    tx.execute(
                        "UPDATE build_items SET sha256 = ?3, size = ?4, dev = ?5, ino = ?6, mtime_ns = ?7
                         WHERE build_id = ?1 AND seq = ?2",
                        params![
                            p.build,
                            w.seq,
                            f.sha,
                            file.size as i64,
                            dev,
                            ino,
                            file.mtime_ns as i64
                        ],
                    )?;
                }
                match built {
                    Built::Found | Built::Published => {
                        if !record_applied(tx, p.build, p.folder, w.seq, item, &f.at, true)? {
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
                crate::placements::place_file_as(
                    tx,
                    p.set,
                    p.folder,
                    &item.target,
                    &item.game,
                    &f.now,
                    f.sha.as_deref(),
                )?;
                if item.chd {
                    record_chd(self, tx, p, w)?;
                }
                if let (Built::Found, [was]) = (built, placement(w)?.originals.as_slice()) {
                    crate::placements::moved_entries(tx, (p.set, p.audit), &was.path, path, was, &f.now)?;
                }
                set_place_state(tx, p.build, w.seq, Some(next))?;
            }
            if !matches!(built, Built::Found) {
                let (from, to) = rewrites(unit, &paths)?;
                crate::placements::rewritten_entries(tx, (p.set, p.audit), &from, &to)?;
            }
            if let Some(key) = key {
                tx.execute(
                    "UPDATE build_units SET state = 'done' WHERE build_id = ?1 AND unit = ?2",
                    params![p.build, key],
                )?;
            }
            Ok(())
        })?;
        for w in unit {
            self.fault_point(Fault::PlaceRecorded(w.seq));
        }
        Ok(())
    }

    /// Every file of the release of `game` this Build recorded placed, each held open now, where it was recorded.
    fn outputs(&self, p: &Placing, game: &str) -> Result<Vec<Output>> {
        let release = release_of(game);
        let rows: Vec<PlacedRow> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT seq, target, game, dev, ino, sha256 FROM build_items
                 WHERE build_id = ?1 AND kind = 'place' AND state = 'applied' ORDER BY seq",
            )?;
            let rows = stmt
                .query_map([p.build], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                })?
                .collect::<rusqlite::Result<_>>()?;
            Ok(rows)
        })?;
        let mut outputs = Vec::new();
        for (seq, target, game, dev, ino, sha) in rows {
            if release_of(&game) != release {
                continue;
            }
            let found = match (dev, ino) {
                (Some(dev), Some(ino)) => match self.recorded_spot_of(p, seq, &target)? {
                    Ok(s) => carry::Held::open(&s.dir, &s.leaf, Identity::from_sql(dev, ino))?
                        .map(|held| (s, held))
                        .map_err(|why| format!("{target}: {why}")),
                    Err(why) => Err(why),
                },
                _ => Err(format!("what {target} was placed as isn't recorded")),
            };
            outputs.push(Output {
                seq,
                target,
                sha,
                found,
            });
        }
        Ok(outputs)
    }

    /// Deletes the Source Set files of each item of `unit`, its release now placed at `spots`. `Err` says why one is
    /// left outstanding.
    fn dispose_unit(
        &self,
        p: &Placing,
        set: &SetDirs,
        unit: &[&Work],
        spots: &[Spot],
    ) -> Result<std::result::Result<(), String>> {
        let outputs = self.outputs(p, &unit[0].item.game)?;
        for (w, s) in unit.iter().zip(spots) {
            if let Err(why) = self.dispose_originals(p, set, w, s, &outputs)? {
                return Ok(Err(why));
            }
        }
        Ok(Ok(()))
    }

    /// Deletes each Source Set file `w`'s Game, now placed at `s`, was placed from and has not yet disposed of, under
    /// the trusted-stamp rule ([`carry::dispose_as`]). One that can't be proven unchanged, or whose placed file is no
    /// longer in place, is moved into the Source Set's kept files instead, and the item says so. `Err` says why one is
    /// left outstanding: set aside to delete, or being kept, and neither deleted nor kept.
    fn dispose_originals(
        &self,
        p: &Placing,
        set: &SetDirs,
        w: &Work,
        s: &Spot,
        outputs: &[Output],
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
            // One a keep began for is kept, before anything else is done with it.
            if let Some(name) = &row.kept {
                match self.finish_keep(p, set, w.seq, &row, name)? {
                    Ok(why) => kept.push(why),
                    Err(why) => return Ok(Err(why)),
                }
                continue;
            }
            let found = match parent_of(&set.dir, &o.path) {
                Ok(found) => Ok(found),
                // Its folder is gone. Set aside already, it goes back into that folder made again, without following a
                // symbolic link; otherwise it is gone with it.
                Err(_) if row.slot.is_some() => parent_made(&set.dir, &o.path).map_err(|e| {
                    format!(
                        "{} is set aside to delete, and its folder in source set {} is gone and could not be made \
                         again ({e}); it is left in the source set's private .deleting folder until it can be",
                        o.path, p.set_name
                    )
                }),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    self.write(|tx| set_original_state(tx, p.build, (w.seq, row.n), "gone"))?;
                    continue;
                }
                Err(e) => Err(format!(
                    "{} can't be reached ({e}), so it is left as it is until it can be",
                    o.path
                )),
            };
            let (parent, name) = match found {
                Ok(found) => found,
                Err(why) => return Ok(Err(why)),
            };
            let dir = parent.as_ref().unwrap_or(&set.dir);
            let original = carry::Original {
                dir,
                name,
                label: &o.path,
                id: Identity::from(o.identity),
                file: o.file.clone(),
                ctime: Some(o.ctime_ns),
                sha256: row.sha256.as_deref(),
                deleting: &set.deleting,
                tally: p.from,
            };
            let intents = DisposeIntents {
                store: self,
                p,
                set,
                seq: w.seq,
                n: row.n,
                target: &w.item.target,
                chain: &s.chain,
                published,
                outputs,
            };
            let outcome = carry::dispose_as(&original, &s.dest(p), &intents, Some(&sha))?;
            self.fault_point(Fault::PlaceDisposed(w.seq));
            let why = match outcome {
                Ok(()) => {
                    self.write(|tx| set_original_state(tx, p.build, (w.seq, row.n), "deleted"))?;
                    continue;
                }
                Err(why) => why,
            };
            let state: String = self.read(|c| {
                Ok(c.query_row(
                    "SELECT state FROM placement_originals WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                    params![p.build, w.seq, row.n],
                    |r| r.get(0),
                )?)
            })?;
            // Kept from its slot, as it was set aside to delete.
            if state == "kept" {
                kept.push(why);
                continue;
            }
            // Set aside to delete, and neither deleted nor kept: never recorded either, until it is one or the other.
            if intents.load()?.slot.is_some() {
                return Ok(Err(why));
            }
            // Never set aside, since it changed or its placed file can't be held: kept from where it is, if it is.
            match dir.stat(name).map_err(io_err)? {
                Some(st) if FolderDir::kind(&st) == FileType::RegularFile => {
                    match self.keep_placed_original(
                        p,
                        set,
                        (w.seq, row.n, &o.path),
                        (dir, name),
                        None,
                        &why,
                    )? {
                        Ok(kept_as) => kept.push(format!("{why}, so it was {kept_as}")),
                        Err(e) => {
                            return Ok(Err(format!(
                                "{why}, and it could not be kept ({e}); it is left as it is until it can be"
                            )));
                        }
                    }
                }
                _ => {
                    self.write(|tx| set_original_state(tx, p.build, (w.seq, row.n), "gone"))?;
                }
            }
        }
        self.write(|tx| {
            if !kept.is_empty() {
                tx.execute(
                    "UPDATE build_items SET error = ?3 WHERE build_id = ?1 AND seq = ?2",
                    params![
                        p.build,
                        w.seq,
                        format!("{} is placed here: {}", w.item.game, kept.join("; "))
                    ],
                )?;
            }
            set_place_state(tx, p.build, w.seq, Some("done"))
        })?;
        Ok(Ok(()))
    }

    /// Moves the Source Set file `path` of item `seq`'s original `n`, `name` in `dir` (where it is, or its slot in the
    /// private `.deleting` folder), into the Source Set's kept files as `kept`, or a free name recorded first, and
    /// records it kept by the Build, because `why`. Returns how it was kept, or why it could not be.
    fn keep_placed_original(
        &self,
        p: &Placing,
        set: &SetDirs,
        (seq, n, path): (i64, i64, &str),
        (dir, name): (&FolderDir, &str),
        kept: Option<(&str, u64)>,
        why: &str,
    ) -> Result<std::result::Result<String, String>> {
        let generation = kept.map_or(p.generation, |(_, g)| g);
        let retention =
            match open_retention(self, Scope::Source(p.set), &set.dir, generation, true)? {
                Ok(Some(retention)) => retention.generation,
                Ok(None) => return Ok(Err("the kept files folder is missing".into())),
                Err(why) => return Ok(Err(why)),
            };
        let st = match dir.stat(name).map_err(io_err)? {
            Some(st) if FolderDir::kind(&st) == FileType::RegularFile => st,
            _ => return Ok(Err(format!("{path} is not where it was left"))),
        };
        let kept = match kept {
            Some((kept, _)) => kept.to_owned(),
            None => {
                let leaf = path.rsplit('/').next().unwrap_or(path);
                let kept = (1..)
                    .map(|nth| kept_name(leaf, nth))
                    .find(|k| matches!(retention.stat(k), Ok(None)))
                    .expect("a free name");
                self.write(|tx| {
                    tx.execute(
                        "UPDATE placement_originals SET kept_name = ?4, kept_generation = ?5, detail = ?6
                         WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                        params![p.build, seq, n, kept, generation as i64, why],
                    )?;
                    Ok(())
                })?;
                kept
            }
        };
        let pin = dir.pin(name, Identity::of(&st));
        if let Err(e) = dir.rename_new(name, &retention, &kept) {
            return Ok(Err(format!(
                "it could not be moved into the kept files: {e}"
            )));
        }
        dir.sync().map_err(io_err)?;
        self.record_kept(
            p,
            (seq, n, path),
            &retention,
            (&kept, generation),
            pin.as_ref(),
            why,
        )
    }

    /// Records the Source Set file `path` of item `seq`'s original `n`, moved into the kept files folder `retention`
    /// of `generation` as `kept`, kept by the Build because `why`: its change time read through `pin`, the descriptor
    /// held across that move, when there is one.
    fn record_kept(
        &self,
        p: &Placing,
        (seq, n, path): (i64, i64, &str),
        retention: &FolderDir,
        (kept, generation): (&str, u64),
        pin: Option<&File>,
        why: &str,
    ) -> Result<std::result::Result<String, String>> {
        retention.sync().map_err(io_err)?;
        let st = match retention.stat(kept).map_err(io_err)? {
            Some(st) if FolderDir::kind(&st) == FileType::RegularFile => st,
            _ => return Ok(Err(format!("{kept} is not in the kept files"))),
        };
        let kept_ctime = pin
            .and_then(|pin| rustix::fs::fstat(pin).ok())
            .filter(|held| Identity::of(held) == Identity::of(&st))
            .map(|held| ctime_ns(&held).to_string());
        let (dev, ino) = Identity::of(&st).to_sql();
        let file = file_stat(&st);
        self.write(|tx| {
            let recorded: Option<i64> = tx
                .query_row(
                    "SELECT id FROM source_kept WHERE source_set_id = ?1 AND generation = ?2 AND kept_name = ?3
                       AND build_id = ?4",
                    params![p.set, generation as i64, kept, p.build],
                    |r| r.get(0),
                )
                .optional()?;
            let id = match recorded {
                Some(id) => id,
                None => {
                    tx.execute(
                        "INSERT INTO source_kept (source_set_id, build_id, generation, name, kept_name, reason, size,
                                                  mtime_ns, dev, ino, kept_at, kept_ctime_ns)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        params![
                            p.set,
                            p.build,
                            generation as i64,
                            path,
                            kept,
                            why,
                            file.size as i64,
                            file.mtime_ns.to_string(),
                            dev,
                            ino,
                            now(),
                            kept_ctime
                        ],
                    )?;
                    tx.last_insert_rowid()
                }
            };
            tx.execute(
                "UPDATE placement_originals SET state = 'kept', kept_id = ?4, detail = ?5
                 WHERE build_id = ?1 AND seq = ?2 AND n = ?3",
                params![p.build, seq, n, id, why],
            )?;
            Ok(())
        })?;
        Ok(Ok(format!(
            "kept in source set {}'s kept files as {kept}",
            p.set_name
        )))
    }

    /// Finishes keeping `row`, item `seq`'s original, that a stop left part way as `kept`: recorded once it is in the
    /// kept files, moved there again while it is still in its slot or where it was. Returns how it was kept, with
    /// why; `Err` says why it is left outstanding.
    fn finish_keep(
        &self,
        p: &Placing,
        set: &SetDirs,
        seq: i64,
        row: &OriginalRow,
        (kept, generation): &(String, u64),
    ) -> Result<std::result::Result<String, String>> {
        let o = &row.original;
        let why = row
            .detail
            .clone()
            .unwrap_or_else(|| format!("{} could not be confirmed unchanged", o.path));
        let retention =
            match open_retention(self, Scope::Source(p.set), &set.dir, *generation, true)? {
                Ok(Some(retention)) => retention.generation,
                Ok(None) => return Ok(Err("the kept files folder is missing".into())),
                Err(why) => return Ok(Err(why)),
            };
        let done = |how: std::result::Result<String, String>| {
            how.map(|how| format!("{why}, so it was {how}"))
                .map_err(|e| format!("{why}, and it could not be kept ({e}); the build keeps it when the service next starts"))
        };
        if retention.stat(kept).map_err(io_err)?.is_some() {
            return Ok(done(self.record_kept(
                p,
                (seq, row.n, &o.path),
                &retention,
                (kept, *generation),
                None,
                &why,
            )?));
        }
        if let Some(slot) = &row.slot
            && set.deleting.stat(slot).map_err(io_err)?.is_some()
        {
            return Ok(done(self.keep_placed_original(
                p,
                set,
                (seq, row.n, &o.path),
                (&set.deleting, slot),
                Some((kept, *generation)),
                &why,
            )?));
        }
        if let Ok((parent, name)) = parent_of(&set.dir, &o.path) {
            let dir = parent.as_ref().unwrap_or(&set.dir);
            if dir
                .stat(name)
                .map_err(io_err)?
                .is_some_and(|st| is_regular(&st, Identity::from(o.identity)))
            {
                return Ok(done(self.keep_placed_original(
                    p,
                    set,
                    (seq, row.n, &o.path),
                    (dir, name),
                    Some((kept, *generation)),
                    &why,
                )?));
            }
        }
        Ok(Err(format!(
            "{} was being kept in source set {}'s kept files, and is neither there nor where it was; find it and \
             put it back in the source set",
            o.path, p.set_name
        )))
    }

    /// Rolls forward or back each release Build `build`, of plan `plan`, was placing when it stopped, once its Library
    /// Folder `folder` is cleaned up: through the folder, with its marker at the store's generation, and the Source
    /// Set's folder. `Err` says why one is left outstanding.
    pub(super) fn recover_placing(
        &self,
        build: i64,
        plan: i64,
        folder: &crate::folders::FolderRow,
        store: String,
    ) -> Result<std::result::Result<(), String>> {
        let roots = &*self.roots();
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
        let dir = match open_writable_folder(roots, &folder.root, &folder.path) {
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
        self.recover_placements(build, &items, (&dir, &expected), folder)
    }

    /// Rolls forward or back each release of Build `build`, of plan `items`, that a stop left part way, through the
    /// Library Folder `dir` and the Source Set's folder, opened now. `Err` says why one is left outstanding.
    fn recover_placements(
        &self,
        build: i64,
        items: &[PlanItem],
        (dir, expected): (&FolderDir, &Marker),
        row: &crate::folders::FolderRow,
    ) -> Result<std::result::Result<(), String>> {
        let roots = &*self.roots();
        let (set, set_name, audit, generation, open): (i64, String, i64, i64, Vec<Stopped>) =
            self.read(|c| {
                let (set, name, audit, generation) = c.query_row(
                    "SELECT f.source_set_id, s.name, b.audit_id, COALESCE(sf.generation, 0)
                     FROM builds b JOIN library_folders f ON f.id = b.folder_id
                     JOIN source_sets s ON s.id = f.source_set_id
                     LEFT JOIN source_folders sf ON sf.source_set_id = s.id WHERE b.id = ?1",
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
                Ok((set, name, audit, generation, open))
            })?;
        if open.is_empty() {
            return Ok(Ok(()));
        }
        let set_dirs = match self.open_placing(set)? {
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
            folder: row.id,
            set,
            set_name: &set_name,
            audit,
            dir,
            expected,
            roots,
            root: &row.root,
            path: &row.path,
            generation: generation as u64,
            to: &tally,
            from: &tally,
        };
        // Each release's items left part way, together: a release is placed whole or not at all.
        let mut works: Vec<(Work, String, String)> = Vec::new();
        for (seq, target, state, item_state) in open {
            let Some(item) = items
                .iter()
                .find(|i| i.target == target && i.place.is_some())
            else {
                return Ok(Err(format!(
                    "{target} was being placed, but its plan no longer names it"
                )));
            };
            works.push((Work { seq, item }, state, item_state));
        }
        let mut releases: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, (w, _, _)) in works.iter().enumerate() {
            releases
                .entry(release_of(&w.item.game))
                .or_default()
                .push(i);
        }
        for members in releases.values() {
            let in_state = |state: &str| -> Vec<&Work> {
                members
                    .iter()
                    .filter(|&&i| works[i].1 == state)
                    .map(|&i| &works[i].0)
                    .collect()
            };
            let (renaming, copying, converting, disposing) = (
                in_state("renaming"),
                in_state("copying"),
                in_state("converting"),
                in_state("disposing"),
            );
            let outcome = if !renaming.is_empty() {
                self.recover_rename(&p, &set_dirs, &renaming)?
            } else if !copying.is_empty() {
                self.recover_copy(&p, &set_dirs, &copying)?
            } else if !converting.is_empty() {
                // Its new files put in place and recorded built, by the Build or by recovery: it is placed. Otherwise
                // nothing of it is in place, and the release stays in its Source Set.
                let applied = members
                    .iter()
                    .filter(|&&i| works[i].1 == "converting")
                    .all(|&i| works[i].2 == "applied");
                match applied {
                    true => match self.recorded_spots(&p, &converting)? {
                        Ok(spots) => {
                            self.finish_convert(&p, &set_dirs, &converting, &spots, false)?
                        }
                        Err(why) => Err(why),
                    },
                    false => {
                        self.fail_unit(&p, &converting, "the build stopped before it was placed")?;
                        Ok(())
                    }
                }
            } else {
                match self.recorded_spots(&p, &disposing)? {
                    Ok(spots) => self.dispose_unit(&p, &set_dirs, &disposing, &spots)?,
                    Err(why) => Err(why),
                }
            };
            if let Err(why) = outcome {
                return Ok(Err(why));
            }
        }
        Ok(Ok(()))
    }

    /// Where each of `unit`'s files is, as [`Store::recorded_spot`]. Returns why one is not where it was put, for the
    /// release to be left outstanding: its Source Set's files are consumed only once it is back.
    fn recorded_spots(
        &self,
        p: &Placing,
        unit: &[&Work],
    ) -> Result<std::result::Result<Vec<Spot>, String>> {
        let mut spots = Vec::new();
        for w in unit {
            match self.recorded_spot(p, w)? {
                Ok(s) => spots.push(s),
                Err(why) => {
                    return Ok(Err(format!(
                        "{} is placed, but {why}; its source set's files are left as they are until the folder is \
                         back, and the build finishes then",
                        w.item.game
                    )));
                }
            }
        }
        Ok(Ok(spots))
    }
}

/// The unit a release placed by converting it commits as, apart from the files of it written fresh beside it.
fn unit_key(game: &str) -> String {
    format!("{} (placed)", release_of(game))
}

/// Syncs the folder `o` was in, in the Source Set's folder, if it is still there.
fn sync_home(set: &SetDirs, o: &PlacedOriginal) -> Result<()> {
    if let Ok((parent, _)) = parent_of(&set.dir, &o.path) {
        parent.as_ref().unwrap_or(&set.dir).sync().map_err(io_err)?;
    }
    Ok(())
}

/// Renames what is at `s` back to `o`'s place in the Source Set's folder, making its folders again, without following
/// a symbolic link, if they are gone; never replacing anything.
fn move_home(set: &SetDirs, o: &PlacedOriginal, s: &Spot) -> io::Result<()> {
    let (parent, name) = parent_made(&set.dir, &o.path)?;
    let home = parent.as_ref().unwrap_or(&set.dir);
    s.dir.rename_new(&s.leaf, home, name)?;
    s.dir.sync()?;
    home.sync()
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

/// Whether `item`'s cue sheet, recorded as `cue`, is kept in the store, as a CD placed as CHD needs: none needed for a
/// DVD. Returns why not.
fn cue_kept(store: &Store, item: &PlanItem, cue: Option<&str>) -> std::result::Result<(), String> {
    if !item.chd || cue_rom(item).is_none() {
        return Ok(());
    }
    match cue {
        Some(sha) => store.kept_cue(sha).map(|_| ()).map_err(|e| {
            format!(
                "the cue sheet of {} kept in the store can't be read: {e}",
                item.game
            )
        }),
        None => Err(format!(
            "no cue sheet of {} is recorded kept in the store",
            item.game
        )),
    }
}

/// The cue sheet among `item`'s ROMs, a CD's.
fn cue_rom(item: &PlanItem) -> Option<&title_select_api::build::ExpectedRom> {
    item.roms
        .iter()
        .find(|r| r.name.to_ascii_lowercase().ends_with(".cue"))
}

/// Records item `w`'s file, placed as a CHD in this transaction `tx`, as one: with the header SHA-1 its built file was
/// recorded with once extracting it matched the DAT, its disc's ROMs, and for a CD the cue sheet its item recorded
/// kept, read back from the store first.
fn record_chd(store: &Store, tx: &Transaction, p: &Placing, w: &Work) -> Result<()> {
    let item = w.item;
    let sha1: Option<String> = tx
        .query_row(
            "SELECT chd_sha1 FROM built_files WHERE folder_id = ?1 AND target = ?2 AND chd",
            params![p.folder, item.target],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let cue: Option<String> = tx.query_row(
        "SELECT cue_sha256 FROM build_items WHERE build_id = ?1 AND seq = ?2",
        params![p.build, w.seq],
        |r| r.get(0),
    )?;
    let Some(sha1) = sha1 else {
        return Err(Error::Io(format!(
            "{} is placed as a CHD, but what its header held when it was checked isn't recorded",
            item.target
        )));
    };
    cue_kept(store, item, cue.as_deref()).map_err(Error::Io)?;
    crate::placements::place_chd(
        tx,
        p.folder,
        &item.target,
        &sha1,
        &item.roms,
        cue.as_deref(),
    )
}
