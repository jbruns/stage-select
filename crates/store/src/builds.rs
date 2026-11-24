//! Builds: carrying out an approved Change Plan in its Library Folder. The folder is opened once without following a
//! symlink, and every change is made through that handle (see [`FolderDir`]). The staging directory, each staged
//! file and each marker temp file are recorded by device and inode before they are relied on, and only an entry with
//! a recorded identity is ever removed. Each file is written and verified in the staging directory, its SHA-256 and
//! identity recorded as the item's publish intent, the marker checked again, and the file renamed into place
//! without replacing anything, only while the staged name is still the verified file; it is recorded applied only
//! if the target is then that file. The store commits the folder's next generation before the marker advances, and
//! recovery moves a marker that did not follow forward, never back. Recovery changes a folder only for a Build the
//! store records as unfinished, only in a writable Root, and only once the folder's marker shows it is that folder.
//! A file the plan keeps aside (a removal, a replacement, or a conflict you chose to keep aside) is moved into the
//! folder's retention area by one rename that never replaces anything, with its intent recorded first (see
//! [`super::kept`]); a file is written over a kept-aside one only once it is recorded kept.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use stage_select_api::build::{
    BuildDetail, BuildItem, BuildSummary, BuiltFile, ExpectedRom, FileStat, GeneratedFile,
    GeneratedKind, IncompleteReason, ItemKind, ItemState, Marker, PlanItem, PlanRemoval,
    PlanSource, RemovalKind, STAGING,
};
use stage_select_api::lifecycle::RegistrationKind;
use stage_select_api::storage::{ChdKind, chd_of};
use stage_select_api::{Job, JobKind, JobState};
use stage_select_engine::build::{
    self, Source, copy, repack, repack_sources, verify, verify_loose, write_loose,
};
use stage_select_engine::chd::{self, Chdman};
use stage_select_engine::meter::{Interruptible, JobMeter, Meter, Metered, Tally, copy_chunked};
use stage_select_engine::{Progress, hex};

use super::checks::Seen;
use super::folder_dir::{FolderDir, Identity, file_stat, parent_of};
use super::folders::{
    MARKER_NEW, MarkerState, build_by_id, builds_where, check_marker, folder_by_id,
    open_writable_folder, read_marker, store_id, write_marker,
};
use super::jobs::finish_running;
use super::kept::{Keep, Original, Retention, create_dir, open_retention, settle_dir_temps};
use super::plans::{digest, json, to_json};
use super::{Error, Fault, Result, Store, now};
use rustix::fs::Stat;

mod cleanup;
mod place;
mod returns;

/// What a Library Folder's current Build cites, in `audit_citations` and `selection_citations`.
fn cited_by(folder: i64) -> String {
    format!("library-folder:{folder}")
}

/// One file to write, as the approved plan lists it.
struct Work<'a> {
    seq: i64,
    item: &'a PlanItem,
}

/// Why a file was not built: its sources or its output, which fails only the item, or the folder, which stops the
/// Build.
enum Failure {
    Item(String),
    Folder(String),
    /// You cancelled the Build while the item was staged, as while chdman was making its CHD: chdman was stopped, or
    /// the reads and writes waiting on a byte-rate cap, and nothing was put in place.
    Cancelled,
}

/// A file written and verified in the staging directory, with its SHA-256 and size, and for a CHD the SHA-1 and the
/// raw data SHA-1 its header recorded once its extraction matched the DAT.
struct Staged {
    sha256: String,
    size: u64,
    /// Its modification time once written and synced.
    mtime_ns: i128,
    chd_sha1: Option<String>,
    chd_raw_sha1: Option<String>,
    /// For a CD made a CHD, its DAT-exact cue sheet, which the CHD doesn't hold.
    cue: Option<Vec<u8>>,
}

/// What a Build needs to make a CHD: chdman, or why it can't be run, the store's scratch directory, and whether you
/// cancelled the Build.
struct ChdWork<'a> {
    store: &'a Store,
    /// Where the bytes written into the Library Folder's Root are counted, and those read back to verify them.
    to: Tally,
    /// Where the bytes read from the Source Set's Root are counted.
    from: Tally,
    /// Where the bytes read from the Additions Root are counted.
    additions: Tally,
    /// Where the bytes written in and read from the store, its scratch directory and its SBI packs, and chdman's, are
    /// counted: for the Job alone.
    scratch: Tally,
    chdman: std::result::Result<Chdman, String>,
    /// How long each run of chdman may take, by the size of the disc.
    limits: chd::Limits,
    tmp: PathBuf,
    build: i64,
    cancelled: &'a dyn Fn() -> bool,
    /// The Library Folders holding the Source Set's Placed Games that the Build reads, each opened without following
    /// a symlink, or why it can't be.
    placed: HashMap<i64, std::result::Result<FolderDir, String>>,
    /// The Source Set's Placed Games' files that are CHDs, by their paths as its Audits name them, and the Source Set.
    placed_chds: HashMap<String, crate::placements::PlacedChd>,
    set: i64,
}

/// The scratch directory, in the store's, where item `seq` of Build `build` makes its CHD.
fn chd_dir(tmp: &Path, build: i64, seq: i64) -> PathBuf {
    tmp.join(format!("chd-{build}-{seq}"))
}

/// Removes every CHD scratch directory Build `build` left in the store's scratch directory `tmp`.
fn remove_chd_dirs(tmp: &Path, build: i64) {
    let prefix = format!("chd-{build}-");
    for entry in std::fs::read_dir(tmp).into_iter().flatten().flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Removes, with everything in it, the scratch directory it holds once dropped: a CHD's work space never outlives
/// its item, whether it was built, failed or cancelled.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Why a placed CHD wasn't extracted.
enum Unextracted {
    /// Extracting it proved it doesn't hold its disc as the DAT lists it, why: a track or image of other bytes, a
    /// header or track list saying it holds more, a header no longer the one verified, its kept cue sheet changed, or
    /// chdman's word that it is damaged. Its Game needs review.
    Content(String),
    /// Anything else, why: chdman missing, stopped at its deadline or crashed, a file that couldn't be read or written,
    /// or one that changed while it was read. It says nothing of what the CHD holds.
    Operational(String),
    /// You cancelled the Build; chdman was stopped.
    Cancelled,
}

/// A placed CHD extracted into a scratch directory of its own, removed once dropped: the file in it for each ROM of its
/// disc, every one checked against the DAT, a CD's cue sheet written from the store.
struct ExtractedChd {
    _scratch: Scratch,
    files: Vec<(String, PathBuf)>,
}

/// Extracts `placed`, a placed CHD held open as `held` while it is the file a plan found, into the new scratch
/// directory `dir`, as a build checks a CHD it made: copied there through `held`, never read where it is by chdman,
/// its header and track list checked first not to say it holds more than its disc, the copy checked to hash to `sha256`
/// when it is given, then extracted there once and every track it gives back matched against the DAT's, by the cue
/// sheet kept in the store, which is written beside them, and its header checked to be the one verified when it was
/// placed. `copied` runs once the copy is made, before chdman extracts it. What is read of `held` is counted to `from`.
fn extract_placed_chd(
    work: &ChdWork,
    placed: &crate::placements::PlacedChd,
    held: &File,
    dir: PathBuf,
    (sha256, from): (Option<&str>, &Tally),
    copied: &dyn Fn(),
) -> std::result::Result<ExtractedChd, Unextracted> {
    use Unextracted::{Content, Operational};
    let chdman = match &work.chdman {
        Ok(chdman) => Chdman {
            deadline: work
                .limits
                .for_bytes(placed.roms.iter().map(|r| r.size).sum()),
            ..chdman.clone()
        },
        Err(why) => return Err(Operational(format!("it can't be extracted: {why}"))),
    };
    // Without the cue sheet it was placed with, a CD can't be extracted as the DAT lists it.
    let cue = match &placed.cue {
        Some(sha) => match work.store.kept_cue_as(sha) {
            Ok(cue) => Some(cue),
            Err(crate::blobs::Unkept::Changed(why)) => return Err(Content(why)),
            Err(crate::blobs::Unkept::Unread(why)) => {
                return Err(Operational(format!(
                    "its cue sheet, kept in the store, can't be read: {why}"
                )));
            }
        },
        None => None,
    };
    let (kind, bins) = chd::disc_of(&placed.game, &placed.roms, cue.as_deref())
        .map_err(|why| Operational(format!("what it holds can't be told: {why}")))?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| Operational(format!("{}: {e}", dir.display())))?;
    let scratch = Scratch(dir.clone());
    // Its header or track list saying it holds more than its disc does is what it holds; anything else is not.
    let copy = chd::copy_for_chdman(held, kind, &bins, &dir.join("out.chd"), from).map_err(
        |why| match why.ends_with("it was not extracted") {
            true => Content(why),
            false => Operational(why),
        },
    )?;
    if sha256.is_some_and(|sha| sha != copy) {
        return Err(Operational("it changed while it was read".into()));
    }
    copied();
    let header = chdman
        .verify(&dir, kind, cue.as_deref(), &placed.roms, work.cancelled)
        .map_err(|stop| match stop {
            chd::Stop::Cancelled => Unextracted::Cancelled,
            chd::Stop::Corrupt(why) => Content(format!(
                "extracting it did not give back the DAT's files: {why}"
            )),
            chd::Stop::Failed(why) => Operational(format!("it couldn't be extracted: {why}")),
        })?;
    if header.sha1 != placed.sha1 {
        return Err(Content(format!(
            "its header's SHA-1 is {}, not the {} it had when it was placed",
            header.sha1, placed.sha1
        )));
    }
    let mut files: Vec<(String, PathBuf)> = match kind {
        ChdKind::Cd => bins
            .iter()
            .enumerate()
            .map(|(n, rom)| {
                (
                    rom.name.clone(),
                    dir.join("x").join(format!("t{}.bin", n + 1)),
                )
            })
            .collect(),
        ChdKind::Dvd => bins
            .iter()
            .map(|rom| (rom.name.clone(), dir.join("x").join("x.iso")))
            .collect(),
    };
    if let Some(cue) = &cue {
        let rom = placed
            .roms
            .iter()
            .find(|r| !bins.iter().any(|b| b.name == r.name))
            .ok_or_else(|| Operational("the DAT lists no cue sheet for it".into()))?;
        let path = dir.join("dat.cue");
        File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut f| io::Write::write_all(&mut f, cue).and_then(|()| f.sync_all()))
            .map_err(|e| Operational(format!("{}: {e}", path.display())))?;
        files.push((rom.name.clone(), path));
    }
    Ok(ExtractedChd {
        _scratch: scratch,
        files,
    })
}

/// Writes `item`'s CHD to `file`, new in the staging directory at `staged`: each ROM of the disc is written loose
/// into a scratch directory from the source the plan found, by descriptor, and checked against the DAT; chdman
/// builds the CHD from them, then extracts it again, and every track it gives back must match the DAT's. Only then
/// is the CHD copied into `file`, with its header's SHA-1, of its data and metadata, to check it by from then on. A CHD whose
/// extraction does not match is never staged; the item fails, and says why.
fn stage_chd(
    item: &PlanItem,
    seq: i64,
    file: &mut File,
    staged: &Path,
    set_fd: &std::result::Result<FolderDir, String>,
    work: &ChdWork,
    built_at: SystemTime,
) -> std::result::Result<Staged, Failure> {
    let sized: Vec<(&str, u64)> = item
        .roms
        .iter()
        .map(|r| (r.name.as_str(), r.size))
        .collect();
    let kind = chd_of(&item.game, &sized).map_err(|why| Failure::Item(why.into()))?;
    let chdman = Chdman {
        deadline: work
            .limits
            .for_bytes(item.roms.iter().map(|r| r.size).sum()),
        ..work.chdman.clone().map_err(Failure::Item)?
    };
    let cancelled = work.cancelled;
    let stop = |s: chd::Stop| match s {
        chd::Stop::Cancelled => Failure::Cancelled,
        chd::Stop::Failed(why) | chd::Stop::Corrupt(why) => Failure::Item(why),
    };
    let dir = chd_dir(&work.tmp, work.build, seq);
    let _ = std::fs::remove_dir_all(&dir);
    let input = dir.join("in");
    std::fs::create_dir_all(&input)
        .map_err(|e| Failure::Item(format!("{}: {e}", input.display())))?;
    let _scratch = Scratch(dir.clone());
    // Each file of the Source Set is opened and read once, for all the ROMs it holds: a ZIP holding every track of
    // the disc is read through once, not once a track.
    let mut by_source: Vec<(&PlanSource, Vec<&ExpectedRom>)> = Vec::new();
    for rom in &item.roms {
        let source = item
            .sources
            .iter()
            .find(|s| s.rom == rom.name)
            .ok_or_else(|| Failure::Item(format!("{}: no source", rom.name)))?;
        match by_source.iter_mut().find(|(s, _)| s.path == source.path) {
            Some((_, roms)) => roms.push(rom),
            None => by_source.push((source, vec![rom])),
        }
    }
    let mut written: Vec<(&ExpectedRom, File)> = Vec::with_capacity(item.roms.len());
    for (source, roms) in &by_source {
        if cancelled() {
            return Err(Failure::Cancelled);
        }
        let opened = open_either(set_fd, &work.placed, source).map_err(Failure::Item)?;
        work.store.fault_point(Fault::ChdSourceRead(seq));
        let dests = roms
            .iter()
            .map(|rom| {
                let path = input.join(&rom.name);
                File::create_new(&path)
                    .map_err(|e| Failure::Item(format!("{}: {e}", path.display())))
            })
            .collect::<std::result::Result<Vec<File>, _>>()?;
        let members: Option<Vec<(&ExpectedRom, &str)>> = roms
            .iter()
            .map(|rom| {
                item.sources
                    .iter()
                    .find(|s| s.rom == rom.name)
                    .and_then(|s| s.member.as_deref())
                    .map(|m| (*rom, m))
            })
            .collect();
        match (members, roms.as_slice(), dests.as_slice()) {
            (Some(members), ..) => {
                let refs: Vec<&File> = dests.iter().collect();
                build::write_loose_members(&refs, &members, &opened, &work.from, &work.scratch)
                    .map(|_| ())
            }
            (None, [rom], [dest]) => write_loose(
                dest,
                &[(rom, 0, None)],
                &[(&opened, &work.from)],
                &work.scratch,
            )
            .map(|_| ()),
            (None, ..) => Err(format!("{}: a loose file holds one ROM", source.path)),
        }
        .map_err(Failure::Item)?;
        written.extend(roms.iter().copied().zip(dests));
    }
    let mut cue = None;
    for (rom, dest) in &written {
        verify_loose(dest, rom, &work.scratch).map_err(Failure::Item)?;
        if rom.name.to_ascii_lowercase().ends_with(".cue") {
            let read = chd::read_cue(dest).map_err(Failure::Item)?;
            // Before chdman reads it: each track it names is then checked once extracted.
            chd::cue_tracks(&read)
                .and_then(|names| chd::cue_matches(&names, &item.roms).map(|_| ()))
                .map_err(Failure::Item)?;
            cue = Some(read);
        }
    }
    drop(written);
    let main = item
        .roms
        .iter()
        .find(|r| {
            let name = r.name.to_ascii_lowercase();
            name.ends_with(".cue") || name.ends_with(".iso")
        })
        .map(|r| r.name.as_str())
        .ok_or_else(|| Failure::Item("a disc is built from its .cue or .iso".into()))?;
    // The pace's share of the hash workers as chdman starts, after its inputs are written and checked, however long
    // that took.
    let chdman = Chdman {
        threads: work.store.chdman_threads_now(),
        ..chdman
    };
    let out = chdman.create(&dir, kind, main, cancelled).map_err(stop)?;
    // The ROMs read are no longer needed once the CHD holds them: what it gives back is checked against the DAT.
    std::fs::remove_dir_all(&input)
        .map_err(|e| Failure::Item(format!("{}: {e}", input.display())))?;
    // The CHD extracted is the one copied and hashed: held open, and unchanged from before it is extracted until it is
    // copied.
    let mut built =
        File::open(&out).map_err(|e| Failure::Item(format!("{}: {e}", out.display())))?;
    let seen = |file: &File| {
        rustix::fs::fstat(file)
            .map(|st| crate::checks::Seen::of(&st))
            .map_err(|e| Failure::Item(format!("{}: {e}", out.display())))
    };
    let before = seen(&built)?;
    let header = chdman
        .verify(&dir, kind, cue.as_deref(), &item.roms, cancelled)
        .map_err(|s| match stop(s) {
            Failure::Item(why) => Failure::Item(format!(
                "its CHD was not put in place, because extracting it did not give back the DAT's files: {why}"
            )),
            other => other,
        })?;
    let _ = std::fs::remove_dir_all(dir.join("x"));
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 16];
    let _reading = work.scratch.reader();
    loop {
        let n = work
            .scratch
            .read_from(&mut built, &mut buf)
            .map_err(|e| Failure::Item(format!("{}: {e}", out.display())))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        io::Write::write_all(&mut Metered::new(&mut *file, &work.to), &buf[..n])
            .map_err(|e| Failure::Folder(format!("{}: {e}", staged.display())))?;
    }
    work.scratch.file_read();
    work.to.file_written();
    let copied = hex(&hasher.finalize());
    work.store.fault_point(Fault::ChdCopied(seq));
    if seen(&built)? != before {
        return Err(Failure::Item(format!(
            "{} changed while it was checked",
            out.display()
        )));
    }
    file.set_modified(built_at)
        .and_then(|()| file.sync_all())
        .map_err(|e| Failure::Folder(format!("{}: {e}", staged.display())))?;
    let (sha256, size) = sha256_of(file, &work.to)
        .map_err(|e| Failure::Folder(format!("{}: {e}", staged.display())))?;
    let again = chd::header(file).map_err(Failure::Item)?;
    if sha256 != copied || again != header {
        return Err(Failure::Item(format!(
            "{} is not the CHD that was checked",
            staged.display()
        )));
    }
    Ok(Staged {
        sha256,
        size,
        mtime_ns: staged_mtime(file)?,
        chd_sha1: Some(header.sha1),
        chd_raw_sha1: Some(header.raw_sha1),
        cue,
    })
}

/// Checks that `found`, a CHD you put in the folder at item `seq`'s target, holds the disc of `game` whose ROMs are
/// `roms`, as a build checks a CHD it made: copied into a scratch directory, and extracted there once by chdman, every
/// track it gives back matched against the DAT's. A CD's cue sheet is not in its CHD, so the DAT's is read from the
/// Source Set, from `cue`, the source the cleanup's plan found, and checked against the DAT; the CHD's tracks are
/// matched by it, and the layout chdman gives back must be its layout. Before anything is copied, its header and a
/// CD's track metadata must not say it holds more than the disc does, and nothing past its size when first seen is
/// copied; while chdman extracts it, it is stopped once it writes more than the DAT's files. `found` must hold
/// still, by `fstat`, from before it is copied until after. Returns the SHA-256 of what was copied, and its header, to
/// check it by from then on; nothing is written to the folder. `found` is counted as read from the Library Folder's
/// Root, `work.to`; its copy as written for the Job alone, `work.scratch`; the cue sheet as read from the Source Set's
/// Root, `work.from`.
fn verify_found_chd(
    found: &File,
    game: &str,
    roms: &[ExpectedRom],
    cue: Option<(&PlanSource, &std::result::Result<FolderDir, String>)>,
    seq: i64,
    work: &ChdWork,
) -> std::result::Result<(String, chd::Header), Failure> {
    let sized: Vec<(&str, u64)> = roms.iter().map(|r| (r.name.as_str(), r.size)).collect();
    let kind = chd_of(game, &sized).map_err(|why| Failure::Item(why.into()))?;
    let seen = |file: &File| {
        rustix::fs::fstat(file)
            .map(|st| crate::checks::Seen::of(&st))
            .map_err(|e| Failure::Item(e.to_string()))
    };
    let before = seen(found)?;
    let chdman = Chdman {
        deadline: work.limits.for_bytes(roms.iter().map(|r| r.size).sum()),
        ..work.chdman.clone().map_err(Failure::Item)?
    };
    let dir = chd_dir(&work.tmp, work.build, seq);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| Failure::Item(format!("{}: {e}", dir.display())))?;
    let _scratch = Scratch(dir.clone());
    let cue = match (kind, cue) {
        (ChdKind::Dvd, _) => None,
        (ChdKind::Cd, None) => {
            return Err(Failure::Item(
                "no source was found for its cue sheet, which its layout is checked against".into(),
            ));
        }
        (ChdKind::Cd, Some((source, set_fd))) => {
            let rom = roms
                .iter()
                .find(|r| r.name == source.rom)
                .ok_or_else(|| Failure::Item(format!("the DAT lists no {}", source.rom)))?;
            let opened = open_either(set_fd, &work.placed, source).map_err(Failure::Item)?;
            let read = dat_cue(
                rom,
                source,
                &opened,
                &dir.join("dat.cue"),
                (&work.from, &work.scratch),
            );
            Some(
                read.map_err(|why| {
                    Failure::Item(format!("its cue sheet in the source set: {why}"))
                })?,
            )
        }
    };
    let bins = match &cue {
        None => roms.iter().collect(),
        Some(cue) => chd::cue_tracks(cue)
            .and_then(|names| chd::cue_matches(&names, roms))
            .map_err(|why| Failure::Item(format!("its cue sheet in the source set: {why}")))?,
    };
    let unfit = |why| Failure::Item(format!("{why}; it was not copied or extracted"));
    chd::fits(found, kind, &bins).map_err(unfit)?;
    let out = dir.join("out.chd");
    let mut copy = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&out)
        .map_err(|e| Failure::Item(format!("{}: {e}", out.display())))?;
    let mut reader = found;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|e| Failure::Item(e.to_string()))?;
    let mut reader = reader.take(before.file.size);
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 16];
    let _reading = work.to.reader();
    loop {
        if (work.cancelled)() {
            return Err(Failure::Cancelled);
        }
        let n = work
            .to
            .read_from(&mut reader, &mut buf)
            .map_err(|e| Failure::Item(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        io::Write::write_all(&mut Metered::new(&mut copy, &work.scratch), &buf[..n])
            .map_err(|e| Failure::Item(format!("{}: {e}", out.display())))?;
    }
    work.to.file_read();
    work.scratch.file_written();
    if seen(found)? != before {
        return Err(Failure::Item("it changed while it was read".into()));
    }
    chd::fits(&copy, kind, &bins).map_err(unfit)?;
    drop(copy);
    let header = chdman
        .verify_adopted(&dir, kind, cue.as_deref(), roms, work.cancelled)
        .map_err(|s| match s {
            chd::Stop::Cancelled => Failure::Cancelled,
            chd::Stop::Failed(why) | chd::Stop::Corrupt(why) => Failure::Item(format!(
                "extracting it did not give back the DAT's files: {why}"
            )),
        })?;
    Ok((hex(&hasher.finalize()), header))
}

/// The DAT-exact cue sheet `rom`, written loose from `source`, opened as `opened`, as the new file `path` in a scratch
/// directory, checked against the DAT, and read: what a CD's CHD doesn't hold. `opened` is counted as read from the
/// Root it is in, `from`; the scratch file as written and read for the Job alone, `scratch`.
fn dat_cue(
    rom: &ExpectedRom,
    source: &PlanSource,
    opened: &File,
    path: &Path,
    (from, scratch): (&Tally, &Tally),
) -> std::result::Result<Vec<u8>, String> {
    let dest = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match source.member.as_deref() {
        Some(member) => {
            build::write_loose_members(&[&dest], &[(rom, member)], opened, from, scratch)
                .map(|_| ())
        }
        None => write_loose(&dest, &[(rom, 0, None)], &[(opened, from)], scratch).map(|_| ()),
    }?;
    verify_loose(&dest, rom, scratch)?;
    chd::read_cue(&dest)
}

/// The SHA-256 and size of `file`, read from its start, each byte read counted to `tally`, of the Root `file` is in.
pub(super) fn sha256_of(mut file: &File, tally: &Tally) -> io::Result<(String, u64)> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0; 1 << 16];
    let mut size = 0;
    let _reading = tally.reader();
    loop {
        let n = tally.read_from(&mut file, &mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hex(&hasher.finalize()), size))
}

/// The SHA-256 of `file`, read through it only while its ctime is `ctime` and its ctime, size and mtime hold still
/// across the read; `None` otherwise. Each byte read is counted to `tally`, of the Root `file` is in.
pub(super) fn still_sha256(file: &File, ctime: i128, tally: &Tally) -> io::Result<Option<String>> {
    let at = |st: &rustix::fs::Stat| {
        (
            crate::folder_dir::ctime_ns(st),
            crate::folder_dir::file_stat(st),
        )
    };
    let before = rustix::fs::fstat(file)?;
    if at(&before).0 != ctime {
        return Ok(None);
    }
    let (sha256, _) = sha256_of(file, tally)?;
    let after = rustix::fs::fstat(file)?;
    Ok((at(&before) == at(&after)).then_some(sha256))
}

/// Reads `keep`, the original that writing `converts_to` converts, whole through one descriptor while its ctime, size
/// and mtime hold still, and records what it read as the conversion's record of it, which nothing updates: before the
/// converted file is put in place, so that the original may later be deleted only while it is still exactly that.
/// Returns why it was not recorded, when it is not the file the plan was made with or did not hold still.
fn record_original(
    store: &Store,
    (dir, tally): (&FolderDir, &Tally),
    folder: i64,
    build: i64,
    keep: &stage_select_api::build::KeepAside,
    converts_to: &str,
) -> Result<std::result::Result<Original, String>> {
    let name = keep.name.as_str();
    let changed = || {
        Ok(Err(format!(
            "{name} changed since you approved the plan; {converts_to} was not written, and {name} stays as it is"
        )))
    };
    let Ok(file) = dir.open_file(name) else {
        return changed();
    };
    let expected =
        |st: &Stat| Identity::of(st) == Identity::from(keep.identity) && file_stat(st) == keep.file;
    let before = match rustix::fs::fstat(&file) {
        Ok(st) if expected(&st) => Seen::of(&st),
        _ => return changed(),
    };
    let Ok((sha256, _)) = sha256_of(&file, tally) else {
        return changed();
    };
    match rustix::fs::fstat(&file) {
        Ok(st) if Seen::of(&st) == before => {}
        _ => return changed(),
    }
    if keep.sha256.as_ref().is_some_and(|s| *s != sha256) {
        return changed();
    }
    let (dev, ino) = before.identity.to_sql();
    store.write(|tx| {
        tx.execute(
            "INSERT INTO conversion_originals (folder_id, build_id, target, converts_to, sha256, size, mtime_ns,
                                               ctime_ns, dev, ino, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                folder,
                build,
                name,
                converts_to,
                sha256,
                before.file.size as i64,
                before.file.mtime_ns as i64,
                before.ctime_ns as i64,
                dev,
                ino,
                now()
            ],
        )?;
        Ok(())
    })?;
    Ok(Ok(Original {
        seen: before,
        sha256,
    }))
}

/// The conversion's record of the original `keep` that writing `converts_to` converted, while `keep` is still the
/// file it recorded, by identity.
pub(super) fn conversion_record(
    c: &rusqlite::Connection,
    folder: i64,
    keep: &stage_select_api::build::KeepAside,
    converts_to: &str,
) -> Result<Option<Original>> {
    let (dev, ino) = Identity::from(keep.identity).to_sql();
    Ok(c.query_row(
        "SELECT sha256, size, mtime_ns, ctime_ns FROM conversion_originals
             WHERE folder_id = ?1 AND target = ?2 AND converts_to = ?3 AND dev = ?4 AND ino = ?5
             ORDER BY id DESC LIMIT 1",
        params![folder, keep.name, converts_to, dev, ino],
        |r| {
            Ok(Original {
                sha256: r.get(0)?,
                seen: Seen {
                    file: FileStat {
                        size: r.get::<_, i64>(1)? as u64,
                        mtime_ns: r.get::<_, i64>(2)? as i128,
                    },
                    identity: Identity::from(keep.identity),
                    ctime_ns: r.get::<_, i64>(3)? as i128,
                },
            })
        },
    )
    .optional()?)
}

/// Opens `source` in the Source Set's folder `set` for reading, through held directory handles that follow no
/// symlink, while it is the regular file the plan found there.
fn open_source(
    set: &std::result::Result<FolderDir, String>,
    source: &PlanSource,
) -> std::result::Result<File, String> {
    let set = set.as_ref().map_err(Clone::clone)?;
    let at = |e: std::io::Error| format!("{}: {e}", source.path);
    let mut parts: Vec<&str> = source.path.split('/').collect();
    let name = parts.pop().unwrap_or_default();
    let mut held: Option<FolderDir> = None;
    for part in parts {
        let sub = held.as_ref().unwrap_or(set).sub(part).map_err(at)?;
        held = Some(sub);
    }
    let file = held.as_ref().unwrap_or(set).open_file(name).map_err(at)?;
    let st = rustix::fs::fstat(&file).map_err(|e| at(e.into()))?;
    let identity = source.identity.map(Identity::from);
    if identity != Some(Identity::of(&st))
        || source.ctime_ns != Some(crate::folder_dir::ctime_ns(&st))
        || file_stat(&st) != source.file
    {
        return Err(format!(
            "{} is not the file the plan found there; plan again",
            source.path
        ));
    }
    Ok(file)
}

/// Opens `source`, a file of a Placed Game, in the Library Folder holding it, one of `placed`, while it is the regular
/// file the plan found there.
fn open_placed_source(
    placed: &HashMap<i64, std::result::Result<FolderDir, String>>,
    source: &PlanSource,
) -> std::result::Result<File, String> {
    let (folder, rel) = stage_select_api::storage::split_placed(&source.path)
        .ok_or_else(|| format!("{} is not a placed file", source.path))?;
    let dir = match placed.get(&folder) {
        Some(Ok(dir)) => dir,
        Some(Err(why)) => return Err(format!("{}: {why}", source.path)),
        None => {
            return Err(format!(
                "{}: its library folder can't be found",
                source.path
            ));
        }
    };
    let file = dir
        .open_file(rel)
        .map_err(|e| format!("{}: {e}", source.path))?;
    let st = rustix::fs::fstat(&file).map_err(|e| format!("{}: {e}", source.path))?;
    if source.identity.map(Identity::from) != Some(Identity::of(&st))
        || source.ctime_ns != Some(crate::folder_dir::ctime_ns(&st))
        || file_stat(&st) != source.file
    {
        return Err(format!(
            "{} is not the file the plan found there; plan again",
            source.path
        ));
    }
    Ok(file)
}

/// Opens `source` where it is: in the Source Set's folder `set`, or, for a Placed Game's file, in the Library Folder
/// holding it, one of `placed`; in either only while it is the regular file the plan found there.
fn open_either(
    set: &std::result::Result<FolderDir, String>,
    placed: &HashMap<i64, std::result::Result<FolderDir, String>>,
    source: &PlanSource,
) -> std::result::Result<File, String> {
    match stage_select_api::storage::split_placed(&source.path) {
        Some(_) => open_placed_source(placed, source),
        None => open_source(set, source),
    }
}

/// Writes `item` to `file`, new in the staging directory, from its sources, each opened by `open` only while it is
/// the file the plan found and checked again once read: copied whole for a copy, else written from its ROMs. Then
/// verifies it.
fn stage_by_fd(
    item: &PlanItem,
    file: &mut File,
    open: &dyn Fn(&PlanSource) -> std::result::Result<File, String>,
    (from, to): (&Tally, &Tally),
    built_at: SystemTime,
) -> std::result::Result<Staged, Failure> {
    let mut paths: Vec<&str> = Vec::new();
    let mut opened: Vec<(File, Stat)> = Vec::new();
    for source in &item.sources {
        if paths.contains(&source.path.as_str()) {
            continue;
        }
        let held = open(source).map_err(Failure::Item)?;
        let st = rustix::fs::fstat(&held).map_err(|e| Failure::Item(e.to_string()))?;
        paths.push(&source.path);
        opened.push((held, st));
    }
    let item_err = |e: io::Error| Failure::Item(format!("{}: {e}", item.target));
    match (item.kind, opened.as_slice()) {
        (ItemKind::Copy, [(held, _)]) => {
            copy_chunked(held, from, file, to).map_err(item_err)?;
            from.file_read();
            to.file_written();
        }
        (ItemKind::Copy, _) => return Err(Failure::Item("a copy is of one file".into())),
        _ => {
            let roms: Vec<build::FromSource> = item
                .roms
                .iter()
                .map(|rom| {
                    let source = item
                        .sources
                        .iter()
                        .find(|s| s.rom == rom.name)
                        .ok_or_else(|| Failure::Item(format!("no source for {}", rom.name)))?;
                    let at = paths
                        .iter()
                        .position(|p| *p == source.path)
                        .expect("opened above");
                    Ok((rom, at, source.member.as_deref()))
                })
                .collect::<std::result::Result<_, Failure>>()?;
            let reading: Vec<(&File, &Tally)> = opened.iter().map(|(f, _)| (f, from)).collect();
            match item.loose {
                true => write_loose(file, &roms, &reading, to),
                false => repack_sources(file, &roms, &reading, to),
            }
            .map_err(Failure::Item)?;
        }
    }
    let staged =
        |e: io::Error| Failure::Folder(format!("the staged file for {}: {e}", item.target));
    file.sync_all().map_err(staged)?;
    for ((held, before), path) in opened.iter().zip(&paths) {
        let after = rustix::fs::fstat(held).map_err(|e| Failure::Item(e.to_string()))?;
        if file_stat(&after) != file_stat(before)
            || crate::folder_dir::ctime_ns(&after) != crate::folder_dir::ctime_ns(before)
        {
            return Err(Failure::Item(format!("{path} changed while it was read")));
        }
    }
    match (item.loose, item.roms.as_slice()) {
        (true, [rom]) => verify_loose(file, rom, to),
        (true, _) => Err("a loose file holds one ROM".into()),
        (false, _) => verify(file, &item.roms, to),
    }
    .map_err(Failure::Item)?;
    file.set_modified(built_at)
        .and_then(|()| file.sync_all())
        .map_err(staged)?;
    let (sha256, size) = sha256_of(file, to).map_err(staged)?;
    Ok(Staged {
        sha256,
        size,
        mtime_ns: staged_mtime(file)?,
        chd_sha1: None,
        chd_raw_sha1: None,
        cue: None,
    })
}

/// Writes `item`, the Build's item `seq`, to `file`, new in the staging directory at `staged`, and verifies it.
fn stage(
    (item, seq): (&PlanItem, i64),
    file: &mut File,
    staged: &Path,
    set: (&Path, &Path, &std::result::Result<FolderDir, String>),
    additions: Option<&FolderDir>,
    chd: &ChdWork,
    built_at: SystemTime,
) -> std::result::Result<Staged, Failure> {
    let (set_dir, set_root, set_fd) = set;
    if let Some(placed) = item
        .sources
        .iter()
        .find_map(|s| chd.placed_chds.get(&s.path))
    {
        return stage_from_placed_chd((item, seq), file, placed, chd, built_at);
    }
    if item.disk {
        return stage_disk((item, seq), file, staged, set_fd, chd, built_at);
    }
    if item.chd {
        return stage_chd(item, seq, file, staged, set_fd, chd, built_at);
    }
    if let Some(generated) = &item.generated {
        return stage_generated(generated, file, staged, chd, built_at);
    }
    if let Some(addition) = &item.addition {
        // An Addition is read from its Additions Set, through a descriptor on the file published there, while
        // it is still that file: copied whole when the play library stores it in the format its Additions Set does,
        // else written from its one ROM in the play library's.
        let source = &item.sources[0];
        let dir = additions.ok_or_else(|| {
            Failure::Item(format!(
                "the additions set holding {} cannot be opened",
                source.path
            ))
        })?;
        let from = dir
            .open_file(&source.path)
            .map_err(|e| Failure::Item(format!("{}: {e}", source.path)))?;
        let changed = || {
            Failure::Item(format!(
                "{} in the additions set is not the file published there; check the additions set",
                source.path
            ))
        };
        let st = rustix::fs::fstat(&from).map_err(|e| Failure::Item(e.to_string()))?;
        if Identity::of(&st) != Identity::from(addition.identity) || file_stat(&st) != source.file {
            return Err(changed());
        }
        let [rom] = item.roms.as_slice() else {
            return Err(Failure::Item("an addition holds exactly one ROM".into()));
        };
        let whole = addition.loose == item.loose;
        if whole {
            copy_chunked(&from, &chd.additions, file, &chd.to)
                .map_err(|e| Failure::Item(format!("{}: {e}", source.path)))?;
            chd.additions.file_read();
            chd.to.file_written();
        } else {
            let read = [(rom, 0, (!addition.loose).then_some(rom.name.as_str()))];
            match item.loose {
                true => write_loose(file, &read, &[(&from, &chd.additions)], &chd.to),
                false => repack_sources(file, &read, &[(&from, &chd.additions)], &chd.to),
            }
            .map_err(Failure::Item)?;
        }
        file.sync_all()
            .map_err(|e| Failure::Folder(format!("{}: {e}", staged.display())))?;
        let after = rustix::fs::fstat(&from).map_err(|e| Failure::Item(e.to_string()))?;
        if Identity::of(&after) != Identity::of(&st) || file_stat(&after) != source.file {
            return Err(changed());
        }
        match item.loose {
            true => verify_loose(file, rom, &chd.to),
            false => verify(file, &item.roms, &chd.to),
        }
        .map_err(Failure::Item)?;
        file.set_modified(built_at)
            .and_then(|()| file.sync_all())
            .map_err(|e| Failure::Folder(format!("{}: {e}", staged.display())))?;
        let (sha256, size) = sha256_of(file, &chd.to)
            .map_err(|e| Failure::Folder(format!("{}: {e}", staged.display())))?;
        if whole && sha256 != addition.sha256 {
            return Err(changed());
        }
        return Ok(Staged {
            sha256,
            size,
            mtime_ns: staged_mtime(file)?,
            chd_sha1: None,
            chd_raw_sha1: None,
            cue: None,
        });
    }
    // A Placed Game is read from the Library Folder holding it, by descriptor.
    if item
        .sources
        .iter()
        .any(|s| stage_select_api::storage::split_placed(&s.path).is_some())
    {
        let open = |source: &PlanSource| open_either(set_fd, &chd.placed, source);
        return stage_by_fd(item, file, &open, (&chd.from, &chd.to), built_at);
    }
    let sources: Vec<PathBuf> = item
        .sources
        .iter()
        .map(|s| {
            let path = set_dir.join(&s.path);
            let real = path
                .canonicalize()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if real.starts_with(set_root) {
                Ok(real)
            } else {
                Err(format!("{}: resolves outside its root", path.display()))
            }
        })
        .collect::<std::result::Result<_, String>>()
        .map_err(Failure::Item)?;
    match item.kind {
        ItemKind::Repack if item.loose => {
            let ([rom], [source]) = (item.roms.as_slice(), item.sources.as_slice()) else {
                return Err(Failure::Item(
                    "a loose file is written from one source for its one ROM".into(),
                ));
            };
            let opened = open_source(set_fd, source).map_err(Failure::Item)?;
            write_loose(
                file,
                &[(rom, 0, source.member.as_deref())],
                &[(&opened, &chd.from)],
                &chd.to,
            )
            .map_err(Failure::Item)?;
        }
        ItemKind::Copy => {
            copy(&sources[0], file, &chd.from, &chd.to)
                .map_err(|e| Failure::Item(format!("{}: {e}", sources[0].display())))?;
        }
        ItemKind::Repack => {
            let pairs: Vec<(&ExpectedRom, Source)> = item
                .roms
                .iter()
                .map(|rom| {
                    let i = item
                        .sources
                        .iter()
                        .position(|s| s.rom == rom.name)
                        .expect("a writable item has a source for every ROM");
                    (
                        rom,
                        Source {
                            path: &sources[i],
                            member: item.sources[i].member.as_deref(),
                        },
                    )
                })
                .collect();
            repack(file, &pairs, &chd.from, &chd.to).map_err(Failure::Item)?;
        }
        _ => unreachable!("only copies and repacks are written"),
    }
    match (item.loose, item.roms.as_slice()) {
        (true, [rom]) => verify_loose(file, rom, &chd.to),
        (true, _) => Err("a loose file holds one ROM".into()),
        (false, _) => verify(file, &item.roms, &chd.to),
    }
    .map_err(Failure::Item)?;
    file.set_modified(built_at)
        .and_then(|()| file.sync_all())
        .map_err(|e| Failure::Folder(format!("{}: {e}", staged.display())))?;
    let (sha256, size) = sha256_of(file, &chd.to)
        .map_err(|e| Failure::Folder(format!("{}: {e}", staged.display())))?;
    Ok(Staged {
        sha256,
        size,
        mtime_ns: staged_mtime(file)?,
        chd_sha1: None,
        chd_raw_sha1: None,
        cue: None,
    })
}

/// Copies `item`, the Build's item `seq`, a Disk of an Arcade DAT's Game, to `file`, new in the staging directory at
/// `staged`, from the CHD its Source Set keeps, opened by descriptor. The copy is the Disk once `chdman verify` proves
/// its data hashes to the SHA-1 its header records, and that SHA-1 is the DAT's: as a ZIP's members are checked against
/// the DAT, never against the source's bytes, which may have changed since the Audit read them.
fn stage_disk(
    (item, seq): (&PlanItem, i64),
    file: &mut File,
    staged: &Path,
    set: &std::result::Result<FolderDir, String>,
    chd: &ChdWork,
    built_at: SystemTime,
) -> std::result::Result<Staged, Failure> {
    let ([rom], [source]) = (item.roms.as_slice(), item.sources.as_slice()) else {
        return Err(Failure::Item(
            "a disk is copied from one CHD for its one disk".into(),
        ));
    };
    let at = |e: io::Error| Failure::Folder(format!("{}: {e}", staged.display()));
    let from = open_either(set, &chd.placed, source).map_err(Failure::Item)?;
    copy_chunked(&from, &chd.from, file, &chd.to)
        .map_err(|e| Failure::Item(format!("{}: {e}", source.path)))?;
    chd.from.file_read();
    chd.to.file_written();
    file.set_modified(built_at)
        .and_then(|()| file.sync_all())
        .map_err(at)?;
    let header =
        chd::header(file).map_err(|why| Failure::Item(format!("{}: {why}", item.target)))?;
    if rom.sha1.as_deref() != Some(header.sha1.as_str()) {
        return Err(Failure::Item(format!(
            "{} does not verify: its CHD header records SHA-1 {}, not the disk's",
            item.target, header.sha1
        )));
    }
    let chdman = Chdman {
        deadline: chd.limits.for_bytes(header.logical_bytes),
        ..chd.chdman.clone().map_err(Failure::Item)?
    };
    let dir = chd_dir(&chd.tmp, chd.build, seq);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| Failure::Item(format!("{}: {e}", dir.display())))?;
    let _scratch = Scratch(dir.clone());
    let input = std::path::absolute(staged).map_err(at)?;
    match chdman.verify_in_place(&dir, &input, chd.cancelled) {
        Ok(()) => {}
        Err(_) if (chd.cancelled)() => return Err(Failure::Cancelled),
        Err(why) => {
            return Err(Failure::Item(format!(
                "{} does not verify: {}",
                item.target,
                why.why()
            )));
        }
    }
    let (sha256, size) = sha256_of(file, &chd.to).map_err(at)?;
    Ok(Staged {
        sha256,
        size,
        mtime_ns: staged_mtime(file)?,
        chd_sha1: Some(header.sha1),
        chd_raw_sha1: Some(header.raw_sha1),
        cue: None,
    })
}

/// Writes `item`, the Build's item `seq`, to `file`, new in the staging directory, from `placed`, the CHD its Game
/// is placed as in the folder holding it, opened by descriptor while it is the file the plan found there. A folder that
/// stores the Game as CHD copies it whole, and the copy must be the CHD verified by extraction when it was placed, by its
/// SHA-256 and its header's; any other is written from what extracting it gives back, every file checked against the
/// DAT, as a return writes it home. One extraction proves isn't its disc as the DAT lists it flags its Game for review.
fn stage_from_placed_chd(
    (item, seq): (&PlanItem, i64),
    file: &mut File,
    placed: &crate::placements::PlacedChd,
    work: &ChdWork,
    built_at: SystemTime,
) -> std::result::Result<Staged, Failure> {
    let path = stage_select_api::storage::placed_path(placed.folder, &placed.path);
    if item.sources.iter().any(|s| s.path != path) {
        return Err(Failure::Item(format!(
            "{} is placed as a CHD, and is built from it alone",
            item.game
        )));
    }
    let source = &item.sources[0];
    let held = open_placed_source(&work.placed, source).map_err(Failure::Item)?;
    let staged =
        |e: io::Error| Failure::Folder(format!("the staged file for {}: {e}", item.target));
    let same_disc =
        item.roms.len() == placed.roms.len() && item.roms.iter().all(|r| placed.roms.contains(r));
    if item.chd {
        if !same_disc {
            return Err(Failure::Item(format!(
                "{} is placed as a CHD of another disc than the DAT lists now; audit the source set",
                source.path
            )));
        }
        let before = rustix::fs::fstat(&held).map_err(|e| Failure::Item(e.to_string()))?;
        copy_chunked(&held, &work.from, file, &work.to)
            .map_err(|e| Failure::Item(format!("{}: {e}", source.path)))?;
        work.from.file_read();
        work.to.file_written();
        file.sync_all().map_err(staged)?;
        let after = rustix::fs::fstat(&held).map_err(|e| Failure::Item(e.to_string()))?;
        if file_stat(&after) != file_stat(&before)
            || crate::folder_dir::ctime_ns(&after) != crate::folder_dir::ctime_ns(&before)
        {
            return Err(Failure::Item(format!(
                "{} changed while it was read",
                source.path
            )));
        }
        let (sha256, size) = sha256_of(file, &work.to).map_err(staged)?;
        if placed.sha256.as_deref() != Some(sha256.as_str()) {
            return Err(Failure::Item(format!(
                "{} is not the CHD verified when it was placed; audit the source set",
                source.path
            )));
        }
        let header = chd::header(file).map_err(Failure::Item)?;
        if header.sha1 != placed.sha1 {
            return Err(Failure::Item(format!(
                "{}: its header's SHA-1 is not the one verified when it was placed",
                source.path
            )));
        }
        file.set_modified(built_at)
            .and_then(|()| file.sync_all())
            .map_err(staged)?;
        return Ok(Staged {
            sha256,
            size,
            mtime_ns: staged_mtime(file)?,
            chd_sha1: Some(header.sha1),
            chd_raw_sha1: Some(header.raw_sha1),
            cue: None,
        });
    }
    let dir = chd_dir(&work.tmp, work.build, seq);
    let extracted = match extract_placed_chd(
        work,
        placed,
        &held,
        dir,
        (placed.sha256.as_deref(), &work.from),
        &|| (),
    ) {
        Ok(extracted) => extracted,
        Err(Unextracted::Content(why)) => {
            let why = format!("{}: {why}", source.path);
            work.store
                .write(|tx| {
                    crate::placements::raise_review(
                        tx,
                        (work.set, &placed.game),
                        &path,
                        &why,
                        work.build,
                    )
                })
                .map_err(|e| Failure::Folder(e.to_string()))?;
            return Err(Failure::Item(why));
        }
        Err(Unextracted::Operational(why)) => {
            return Err(Failure::Item(format!("{}: {why}", source.path)));
        }
        Err(Unextracted::Cancelled) => return Err(Failure::Cancelled),
    };
    let mut from = item.clone();
    for source in &mut from.sources {
        let Some((_, at)) = extracted.files.iter().find(|(rom, _)| *rom == source.rom) else {
            return Err(Failure::Item(format!(
                "extracting {} gave back no {}",
                source.path, source.rom
            )));
        };
        source.path = at.display().to_string();
        source.member = None;
    }
    let ours: Vec<String> = extracted
        .files
        .iter()
        .map(|(_, at)| at.display().to_string())
        .collect();
    let open = |source: &PlanSource| match ours.contains(&source.path) {
        true => File::open(&source.path).map_err(|e| format!("{}: {e}", source.path)),
        false => Err(format!(
            "{} is not what extracting the CHD gave back",
            source.path
        )),
    };
    stage_by_fd(&from, file, &open, (&work.scratch, &work.to), built_at)
}

/// Writes `generated`, a file no ROM is, to `file`, new in the staging directory at `staged`: an `.m3u` from its
/// text, or an SBI file from its pack, kept in `store`. It must be the file the plan was made with.
fn stage_generated(
    generated: &GeneratedFile,
    file: &mut File,
    staged: &Path,
    work: &ChdWork,
    built_at: SystemTime,
) -> std::result::Result<Staged, Failure> {
    let store = work.store;
    let bytes = match (
        generated.kind,
        &generated.text,
        &generated.pack,
        &generated.member,
    ) {
        (GeneratedKind::M3u, Some(text), ..) => text.clone().into_bytes(),
        (GeneratedKind::Sbi, _, Some(pack), Some(member)) => store
            .sbi_member(pack, member, &generated.sha256, &work.scratch)
            .map_err(|e| Failure::Item(e.to_string()))?,
        _ => {
            return Err(Failure::Item(
                "the plan does not say what the file holds".into(),
            ));
        }
    };
    let at = |e: io::Error| Failure::Folder(format!("{}: {e}", staged.display()));
    Metered::new(&mut *file, &work.to)
        .write_all(&bytes)
        .map_err(at)?;
    work.to.file_written();
    file.set_modified(built_at)
        .and_then(|()| file.sync_all())
        .map_err(at)?;
    let (sha256, size) = sha256_of(file, &work.to).map_err(at)?;
    if sha256 != generated.sha256 || size != generated.size {
        return Err(Failure::Item(
            "it is not the file the plan was made with".into(),
        ));
    }
    Ok(Staged {
        sha256,
        size,
        mtime_ns: staged_mtime(file)?,
        chd_sha1: None,
        chd_raw_sha1: None,
        cue: None,
    })
}

/// Makes the disc layout folder `target` goes in, when it is in one that is missing, so that it can be renamed into
/// place. Returns the folder made, by name and identity, for the Build to record as its own: one already there,
/// yours or an earlier Build's, is used as it is.
fn make_parents(dir: &FolderDir, target: &str) -> io::Result<Option<(String, Identity)>> {
    let Some((parent, _)) = target.rsplit_once('/') else {
        return Ok(None);
    };
    match dir.mkdir(parent) {
        Ok(()) => {
            dir.sync()?;
            let st = dir.stat(parent)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{parent} vanished once it was made"),
                )
            })?;
            Ok(Some((parent.to_owned(), Identity::of(&st))))
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(None),
        Err(e) => Err(e),
    }
}

/// Records `made`, a disc layout folder a Build made in Library Folder `folder`, as one it may remove once empty.
fn record_layout_dir(store: &Store, folder: i64, made: Option<(String, Identity)>) -> Result<()> {
    let Some((name, id)) = made else {
        return Ok(());
    };
    let (dev, ino) = id.to_sql();
    store.write(|tx| {
        tx.execute(
            "INSERT OR REPLACE INTO folder_dirs (folder_id, name, dev, ino) VALUES (?1, ?2, ?3, ?4)",
            params![folder, name, dev, ino],
        )?;
        Ok(())
    })
}

/// The disc layout folder `target`, a file adopted where it is, is in, by name and identity: adopted with it, as one
/// a build made, it is removed once empty.
fn adopted_parent(dir: &FolderDir, target: &str) -> io::Result<Option<(String, Identity)>> {
    let Some((parent, _)) = target.rsplit_once('/') else {
        return Ok(None);
    };
    Ok(dir
        .stat(parent)?
        .filter(|st| FolderDir::kind(st) == rustix::fs::FileType::Directory)
        .map(|st| (parent.to_owned(), Identity::of(&st))))
}

/// Records `parent`, the disc layout folder of a file adopted into Library Folder `folder`, as [`adopted_parent`]
/// found it, unless a build's folder is recorded there already.
fn record_adopted_parent(
    tx: &Transaction,
    folder: i64,
    parent: Option<&(String, Identity)>,
) -> Result<()> {
    if let Some((name, id)) = parent {
        let (dev, ino) = id.to_sql();
        tx.execute(
            "INSERT OR IGNORE INTO folder_dirs (folder_id, name, dev, ino) VALUES (?1, ?2, ?3, ?4)",
            params![folder, name, dev, ino],
        )?;
    }
    Ok(())
}

/// Removes the disc layout folder `target` was in once it is empty, but only one a Build made, recorded by
/// [`record_layout_dir`], and still that very folder: one you made, or made again since, stays. Best effort.
fn remove_layout_dir(store: &Store, dir: &FolderDir, folder: i64, target: &str) -> Result<()> {
    let Some((parent, _)) = target.rsplit_once('/') else {
        return Ok(());
    };
    let Some(made) = store
        .read(|c| super::kept::folder_dirs(c, folder))?
        .get(parent)
        .copied()
    else {
        return Ok(());
    };
    let forget = || {
        store.write(|tx| {
            tx.execute(
                "DELETE FROM folder_dirs WHERE folder_id = ?1 AND name = ?2",
                params![folder, parent],
            )?;
            Ok(())
        })
    };
    match dir.stat(parent) {
        Ok(None) => forget(),
        Ok(Some(st))
            if FolderDir::kind(&st) == rustix::fs::FileType::Directory
                && Identity::of(&st) == made =>
        {
            if dir.rmdir(parent).is_ok() {
                let _ = dir.sync();
                forget()?;
            }
            Ok(())
        }
        // Something else is there now: the folder recorded is gone, whatever took its name.
        Ok(Some(_)) => forget(),
        Err(_) => Ok(()),
    }
}

/// Syncs the disc layout folder `target` was renamed into, if it is in one.
fn sync_parent(dir: &FolderDir, target: &str) -> io::Result<()> {
    match parent_of(dir, target)? {
        (Some(parent), _) => parent.sync(),
        (None, _) => Ok(()),
    }
}

/// The modification time of the staged file `file`, as written and synced.
fn staged_mtime(file: &File) -> std::result::Result<i128, Failure> {
    rustix::fs::fstat(file)
        .map(|st| file_stat(&st).mtime_ns)
        .map_err(|e| Failure::Folder(e.to_string()))
}

/// What was recorded of a CHD as it was verified: its header's SHA-1, and the SHA-256 and stamp of the file put in
/// place.
struct Verified {
    sha1: String,
    sha256: String,
    identity: Identity,
    file: FileStat,
}

/// Whether the file a conversion wrote for `item` is in place as it was recorded built, and, read through one
/// descriptor while it holds still, holds exactly the game's ROMs as the item stores them. `Err` says why not; an
/// error stops the Build.
fn converted_verified(
    dir: &FolderDir,
    item: &PlanItem,
    chd: Option<&Verified>,
    tally: &Tally,
) -> std::result::Result<std::result::Result<(), String>, Error> {
    let io = |e: io::Error| Error::Io(e.to_string());
    let target = &item.target;
    let file = match dir.open_file(target) {
        Ok(file) => file,
        Err(e) => return Ok(Err(format!("{target} could not be read: {e}"))),
    };
    let before = rustix::fs::fstat(&file).map_err(|e| io(e.into()))?;
    let recorded = match dir.stat(target).map_err(io)? {
        Some(st) => {
            Identity::of(&st) == Identity::of(&before) && file_stat(&st) == file_stat(&before)
        }
        None => false,
    };
    if !recorded {
        return Ok(Err(format!("{target} changed once it was written")));
    }
    let verified = match (item.chd, item.loose, item.roms.as_slice()) {
        // A CHD was checked by extracting it once it was built. Before its original is deleted it is read whole again
        // through this descriptor: it is the same CHD while its stamp is the one recorded then, its SHA-256 is too,
        // and its header says so. An overwrite that puts the modification time back is caught by the SHA-256.
        (true, ..) => match chd {
            None => Err("there is no record of it as it was checked".into()),
            Some(v) if Identity::of(&before) != v.identity || file_stat(&before) != v.file => Err(
                "it is not the file that was checked: its size or modification time changed".into(),
            ),
            Some(v) => match sha256_of(&file, tally).map_err(io)? {
                (sha256, size) if sha256 != v.sha256 || size != v.file.size => {
                    Err("it is not the file that was checked: its SHA-256 changed".into())
                }
                _ => match chd::header(&file) {
                    Ok(h) if h.sha1 == v.sha1 => Ok(()),
                    Ok(_) => {
                        Err("its CHD header is not the one recorded when it was checked".into())
                    }
                    Err(why) => Err(why),
                },
            },
        },
        (false, true, [rom]) => verify_loose(&file, rom, tally),
        (false, true, _) => Err("a loose file holds one ROM".into()),
        (false, false, _) => verify(&file, &item.roms, tally),
    };
    let after = rustix::fs::fstat(&file).map_err(|e| io(e.into()))?;
    if crate::checks::Seen::of(&before) != crate::checks::Seen::of(&after) {
        return Ok(Err(format!("{target} changed while it was verified")));
    }
    Ok(verified.map_err(|why| format!("{target} does not verify: {why}")))
}

/// Whether `name` in `dir` is, without following a symlink, the regular file `id`.
fn is_file(dir: &FolderDir, name: &str, id: Identity) -> std::result::Result<bool, String> {
    Ok(dir
        .stat(name)
        .map_err(|e| e.to_string())?
        .is_some_and(|st| {
            FolderDir::kind(&st) == rustix::fs::FileType::RegularFile && Identity::of(&st) == id
        }))
}

/// Why an entry a Build did not create was left in the folder.
fn not_ours(dir: &FolderDir, name: &str) -> String {
    format!(
        "{}/{name} is not something a build of this store created; it was left as it is. Remove it, then restart \
         the service",
        dir.path().display()
    )
}

/// Removes the staged file `name` from `staging` if it is still the file `id`; anything else there is left.
fn remove_staged(staging: &FolderDir, name: &str, id: Identity) -> std::result::Result<(), String> {
    match staging.stat(name).map_err(|e| e.to_string())? {
        None => Ok(()),
        Some(_) if is_file(staging, name, id)? => staging.unlink(name).map_err(|e| e.to_string()),
        Some(_) => Err(not_ours(staging, name)),
    }
}

/// Removes the staging directory `staging`, opened from `dir` and created as `own`, once every entry in it is a file
/// a Build created, by the identities in `known`. Anything else, or a staging directory that is no longer `own`,
/// is left as it is, and the error says so.
fn clear_staging(
    dir: &FolderDir,
    staging: &FolderDir,
    own: Identity,
    known: &HashMap<String, Identity>,
) -> std::result::Result<(), String> {
    if staging.identity().map_err(|e| e.to_string())? != own {
        return Err(not_ours(dir, STAGING));
    }
    for (name, _) in staging.entries().map_err(|e| e.to_string())? {
        match known.get(&name) {
            Some(&id) => remove_staged(staging, &name, id)?,
            None => return Err(not_ours(staging, &name)),
        }
    }
    match dir.stat(STAGING).map_err(|e| e.to_string())? {
        Some(st)
            if FolderDir::kind(&st) == rustix::fs::FileType::Directory
                && Identity::of(&st) == own => {}
        _ => return Err(not_ours(dir, STAGING)),
    }
    dir.rmdir(STAGING)
        .and_then(|()| dir.sync_holding(STAGING))
        .map_err(|e| e.to_string())
}

/// The name a staged file is recorded under in `folder_temps`.
fn staged_temp(name: &str) -> String {
    format!("{STAGING}/{name}")
}

/// The entries Builds and recovery created in Library Folder `folder`, by name.
pub(super) fn temps(c: &Connection, folder: i64) -> Result<HashMap<String, Identity>> {
    let mut stmt = c.prepare("SELECT name, dev, ino FROM folder_temps WHERE folder_id = ?1")?;
    let rows = stmt.query_map([folder], |r| {
        Ok((r.get(0)?, Identity::from_sql(r.get(1)?, r.get(2)?)))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Records, and commits, that the entry `name` in Library Folder `folder` was created as `id`.
pub(super) fn record_temp(store: &Store, folder: i64, name: &str, id: Identity) -> Result<()> {
    let (dev, ino) = id.to_sql();
    store.write(|tx| {
        tx.execute(
            "INSERT OR REPLACE INTO folder_temps (folder_id, name, dev, ino) VALUES (?1, ?2, ?3, ?4)",
            params![folder, name, dev, ino],
        )?;
        Ok(())
    })
}

/// Records the item `seq` of Build `build` applied and its file built, if `found`, the `lstat` or `fstat` of what is
/// at its target, is the regular file its publish intent records. Otherwise it records the item uncertain, leaving
/// the file, and returns false.
fn record_applied(
    tx: &Transaction,
    build: i64,
    folder: i64,
    seq: i64,
    item: &PlanItem,
    found: &Stat,
    torrentzip: bool,
) -> Result<bool> {
    let (dev, ino): (Option<i64>, Option<i64>) = tx.query_row(
        "SELECT dev, ino FROM build_items WHERE build_id = ?1 AND seq = ?2",
        params![build, seq],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let recorded = dev.zip(ino).map(|(dev, ino)| Identity::from_sql(dev, ino));
    if FolderDir::kind(found) != rustix::fs::FileType::RegularFile
        || recorded != Some(Identity::of(found))
    {
        set_item(
            tx,
            build,
            seq,
            ItemState::Uncertain,
            &format!(
                "the file at {} is not the one the build put there; it was left as it is",
                item.target
            ),
        )?;
        return Ok(false);
    }
    let file = file_stat(found);
    tx.execute(
        "UPDATE build_items SET state = 'applied', error = NULL WHERE build_id = ?1 AND seq = ?2",
        params![build, seq],
    )?;
    tx.execute(
        "INSERT INTO built_files (folder_id, target, game, kind, build_id, selection_id, audit_id, dat_revision_id,
                                  roms, sources, size, mtime_ns, sha256, torrentzip, loose, addition_id, chd,
                                  chd_sha1, chd_raw_sha1, disk)
         SELECT ?1, ?2, ?3, ?4, b.id, b.selection_id, b.audit_id, b.dat_revision_id, ?5, ?6, ?7, ?8, i.sha256, ?11,
                ?12, ?13, ?14, CASE WHEN ?14 THEN i.chd_sha1 END, CASE WHEN ?14 THEN i.chd_raw_sha1 END, ?15
         FROM builds b JOIN build_items i ON i.build_id = b.id AND i.seq = ?10 WHERE b.id = ?9
         ON CONFLICT (folder_id, target) DO UPDATE SET
             game = excluded.game, kind = excluded.kind, build_id = excluded.build_id,
             selection_id = excluded.selection_id, audit_id = excluded.audit_id,
             dat_revision_id = excluded.dat_revision_id, roms = excluded.roms, sources = excluded.sources,
             size = excluded.size, mtime_ns = excluded.mtime_ns, sha256 = excluded.sha256,
             torrentzip = excluded.torrentzip, loose = excluded.loose, addition_id = excluded.addition_id,
             chd = excluded.chd, chd_sha1 = excluded.chd_sha1, chd_raw_sha1 = excluded.chd_raw_sha1,
             disk = excluded.disk",
        params![
            folder,
            item.target,
            item.game,
            item.kind.as_str(),
            to_json(&item.roms),
            to_json(&item.sources),
            file.size as i64,
            file.mtime_ns as i64,
            build,
            seq,
            torrentzip && !item.loose && !item.chd,
            item.loose,
            item.addition.as_ref().map(|a| a.id),
            item.chd,
            item.disk
        ],
    )?;
    // A file kept aside to be repacked is no longer missing once a file is built at its name.
    tx.execute(
        "DELETE FROM build_incomplete
         WHERE build_id = (SELECT current_build_id FROM library_folders WHERE id = ?1)
           AND lower(target) = lower(?2) AND reason = 'failed' AND detail = ?3",
        params![folder, item.target, super::kept::REPACK_UNPUBLISHED],
    )?;
    Ok(true)
}

fn set_item(tx: &Transaction, build: i64, seq: i64, state: ItemState, error: &str) -> Result<()> {
    tx.execute(
        "UPDATE build_items SET state = ?3, error = ?4 WHERE build_id = ?1 AND seq = ?2",
        params![build, seq, state.as_str(), error],
    )?;
    Ok(())
}

fn build_items(c: &Connection, build: i64) -> Result<Vec<BuildItem>> {
    let mut stmt = c.prepare(
        "SELECT target, game, kind, state, error FROM build_items WHERE build_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt.query_map([build], |r| {
        let kind: String = r.get(2)?;
        let state: String = r.get(3)?;
        Ok(BuildItem {
            target: r.get(0)?,
            game: r.get(1)?,
            kind: ItemKind::parse(&kind).unwrap_or(ItemKind::Repack),
            state: ItemState::parse(&state).unwrap_or(ItemState::Failed),
            error: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Ends the Running Build `build` of Job `job` as Failed, and the Job as Failed with `error`.
/// Why an item a cancelled Build never reached was not done.
const CANCELLED_ITEM: &str = "you cancelled the build before it got to this";

/// How a cancelled Build's Job ends.
const CANCELLED_BUILD: &str = "you cancelled the build; each file it had put in place or set aside stays so, and \
                               a new plan finishes the folder";

/// Ends Build `build` of `folder`, and its Job `job`, which you cancelled between two of its items: each item it did
/// stays done, and each it never got to says so. The folder stays at its generation; nothing is left to recover.
pub(super) fn end_cancelled(
    tx: &Transaction,
    meter: &Meter,
    job: i64,
    build: i64,
    folder: i64,
) -> Result<()> {
    tx.execute(
        "UPDATE build_items SET error = ?2 WHERE build_id = ?1 AND state = 'pending'",
        params![build, CANCELLED_ITEM],
    )?;
    tx.execute(
        "UPDATE builds SET state = 'interrupted', finished_at = ?2, cleaned_at = ?2 WHERE id = ?1",
        params![build, now()],
    )?;
    tx.execute("DELETE FROM folder_temps WHERE folder_id = ?1", [folder])?;
    finish_running(tx, meter, job, JobState::Interrupted, Some(CANCELLED_BUILD))
}

/// The state of item `seq` of Build `build`.
fn item_state(store: &Store, build: i64, seq: i64) -> Result<Option<String>> {
    store.read(|c| {
        Ok(c.query_row(
            "SELECT state FROM build_items WHERE build_id = ?1 AND seq = ?2",
            params![build, seq],
            |r| r.get(0),
        )
        .optional()?)
    })
}

/// Records on item `seq` of Build `build` what became of the original it disposed of, or why it was left in place.
fn record_disposal(
    store: &Store,
    build: i64,
    seq: i64,
    outcome: std::result::Result<Option<String>, String>,
) -> Result<()> {
    store.write(|tx| {
        match outcome {
            Ok(None) => {}
            Ok(Some(note)) => {
                tx.execute(
                    "UPDATE build_items SET error = ?3 WHERE build_id = ?1 AND seq = ?2",
                    params![build, seq, note],
                )?;
            }
            Err(error) => set_item(tx, build, seq, ItemState::Failed, &error)?,
        }
        Ok(())
    })
}

/// Records the Running Build `build` failed, and the `error` its Job `job` ends with, before the Job ends: a start
/// that finds the Job still Running, stopped while the Build recovered its folder, ends it Failed with that error.
fn fail_build(tx: &Transaction, job: i64, build: i64, error: &str) -> Result<()> {
    tx.execute(
        "UPDATE builds SET state = 'failed', finished_at = ?2 WHERE id = ?1 AND state = 'running'",
        params![build, now()],
    )?;
    tx.execute(
        "UPDATE jobs SET error = ?2, version = version + 1 WHERE id = ?1 AND state = 'running'",
        params![job, error],
    )?;
    Ok(())
}

/// Ends the Build Job `job` Failed with `error`; one no longer Running is left as it is.
fn end_failed(tx: &Transaction, meter: &Meter, job: i64, error: &str) -> Result<()> {
    match finish_running(tx, meter, job, JobState::Failed, Some(error)) {
        Err(Error::Io(_)) | Ok(()) => Ok(()),
        Err(err) => Err(err),
    }
}

/// Checks, through `dir`, that the file at `item`'s target holds exactly the ROMs the DAT expects there, and is the
/// same file, unchanged, from before it is read until after. Nothing is written. Returns its SHA-256, its `lstat`,
/// and whether it is TorrentZip; or why it cannot be adopted, leaving it as it is.
fn adopt(
    (dir, library): (&FolderDir, &Tally),
    item: &PlanItem,
) -> std::result::Result<(String, Stat, bool), String> {
    let left = |why: &str| {
        format!(
            "{} cannot be adopted: {why}; it was left as it is",
            item.target
        )
    };
    let seen = dir
        .stat(&item.target)
        .map_err(|e| left(&e.to_string()))?
        .ok_or_else(|| left("it is gone"))?;
    if item.existing.as_ref() != Some(&file_stat(&seen)) {
        return Err(left("it changed since you approved the plan"));
    }
    let file = dir
        .open_file(&item.target)
        .map_err(|e| left(&e.to_string()))?;
    let before = rustix::fs::fstat(&file).map_err(|e| left(&e.to_string()))?;
    if Identity::of(&before) != Identity::of(&seen) || file_stat(&before) != file_stat(&seen) {
        return Err(left("it changed while it was read"));
    }
    if item.chd {
        return Err(left(
            "only a CHD this store built is recorded as one, once its extraction matched the DAT",
        ));
    }
    // A generated file is known by its whole SHA-256; a ROM, by its payload under the DAT's Header Rule.
    let rule = match item.generated {
        Some(_) => None,
        None => item.roms.iter().find_map(|rom| rom.header),
    };
    let probe = stage_select_engine::build::probe_under(&file, rule, library)
        .map_err(|e| left(&e.to_string()))?;
    match &item.generated {
        Some(generated) if probe.sha256 != generated.sha256 => {
            return Err(left("it is not exactly the file a build writes there"));
        }
        Some(_) => {}
        None if !stage_select_engine::build::holds_as(&probe, &item.roms, item.loose) => {
            return Err(left(
                "it does not hold exactly the ROMs the DAT expects there",
            ));
        }
        None => {}
    }
    let after = rustix::fs::fstat(&file).map_err(|e| left(&e.to_string()))?;
    match dir.stat(&item.target) {
        Ok(Some(st))
            if FolderDir::kind(&st) == rustix::fs::FileType::RegularFile
                && Identity::of(&st) == Identity::of(&before)
                && file_stat(&st) == file_stat(&before)
                && file_stat(&after) == file_stat(&before) =>
        {
            Ok((probe.sha256, st, probe.torrentzip))
        }
        _ => Err(left("it changed while it was read")),
    }
}

/// Records what the Build `build` left its Library Folder lacking of its Selection: each item of its plan that was
/// skipped, other than an unresolved one whose target is `retained` (its earlier built file still holds the ROMs the
/// item now expects), and each item that failed.
fn record_incomplete(
    tx: &Transaction,
    build: i64,
    items: &[PlanItem],
    retained: &HashSet<String>,
) -> Result<()> {
    let failed: HashMap<String, Option<String>> = {
        let mut stmt = tx.prepare(
            "SELECT target, error FROM build_items WHERE build_id = ?1 AND state IN ('failed', 'uncertain', 'pending')",
        )?;
        let rows = stmt.query_map([build], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    let mut seq = 0;
    for item in items {
        let reason = match item.kind {
            _ if failed.contains_key(&item.target) => IncompleteReason::Failed,
            // Left out, but only complete when the file built before still holds the ROMs the item now expects.
            ItemKind::Unresolved if retained.contains(&item.target) => continue,
            ItemKind::Unresolved => IncompleteReason::Unresolved,
            ItemKind::NoSource if !item.missing_dependencies.is_empty() => {
                IncompleteReason::MissingDependency
            }
            ItemKind::NoSource => IncompleteReason::NoSource,
            ItemKind::Conflict => IncompleteReason::Conflict,
            ItemKind::Blocked => IncompleteReason::Blocked,
            _ => continue,
        };
        let detail = match reason {
            IncompleteReason::Failed => failed[&item.target].clone(),
            _ => item.reason.clone(),
        };
        tx.execute(
            "INSERT INTO build_incomplete (build_id, seq, reason, grp, game, target, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                build,
                seq,
                reason.as_str(),
                item.group,
                item.game,
                item.target,
                detail
            ],
        )?;
        seq += 1;
    }
    Ok(())
}

/// An item that was being renamed into place when its Build stopped.
struct Publishing {
    seq: i64,
    target: String,
    sha256: Option<String>,
    size: Option<i64>,
    kind: String,
    /// For a cleanup's move within the folder: where the entry is moved from, its identity, and the kept file it is.
    origin: Option<String>,
    dev: Option<i64>,
    ino: Option<i64>,
    kept_id: Option<i64>,
    /// The release whose discs it is put in place with, as one unit.
    unit: Option<String>,
}

/// Why the discs of `release`, committed to go in place as one unit, are not all in place: `target` could not be
/// kept aside from, or put in, the folder.
fn unit_stuck(release: &str, target: &str, error: &str) -> String {
    format!(
        "the discs of {release} go in place together, but {target} could not be put in place: {error}. Its other \
         discs, the checked files waiting to go in place and anything set aside were all kept, and the build \
         finishes {release} when the service next starts, once {target} can be put in place"
    )
}

/// Renames into place the staged file of `p`, a disc of a unit, whose target is missing, while the staged file is the
/// one the Build verified: the same file, SHA-256 and size. `Err` says why it can't be, leaving both as they are.
fn finish_unit(
    (dir, library): (&FolderDir, &Tally),
    temps: &HashMap<String, Identity>,
    p: &Publishing,
) -> std::result::Result<Option<(String, Identity)>, String> {
    let gone =
        || Err("its checked file is no longer waiting in the build's staging folder".to_string());
    let (Some(dev), Some(ino), Some(sha256), Some(size)) = (p.dev, p.ino, &p.sha256, p.size) else {
        return gone();
    };
    let staging = match dir.stat(STAGING).map_err(|e| e.to_string())? {
        Some(st)
            if FolderDir::kind(&st) == rustix::fs::FileType::Directory
                && temps.get(STAGING) == Some(&Identity::of(&st)) =>
        {
            dir.sub(STAGING).map_err(|e| e.to_string())?
        }
        _ => return gone(),
    };
    let name = format!("{}.zip", p.seq);
    let file = match staging.open_file(&name) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return gone(),
        Err(e) => return Err(e.to_string()),
    };
    if Identity::of_fd(&file).map_err(|e| e.to_string())? != Identity::from_sql(dev, ino) {
        return gone();
    }
    let (hash, len) = sha256_of(&file, library).map_err(|e| e.to_string())?;
    if &hash != sha256 || len as i64 != size {
        return Err("its staged file is not the one that was checked".into());
    }
    let made = make_parents(dir, &p.target).map_err(|e| e.to_string())?;
    staging
        .rename_new(&name, dir, &p.target)
        .and_then(|()| sync_parent(dir, &p.target))
        .map_err(|e| e.to_string())?;
    Ok(made)
}

/// What recovery reads about an unfinished Build.
struct Unfinished {
    plan: i64,
    generation: u64,
    finalizing: bool,
    publishing: Vec<Publishing>,
}

impl Store {
    /// Carries out the approved plan of the Running Build Job `id`, calling `progress` as files are written. The plan
    /// is computed again first and must still have the approved digest; when it does not, the new plan is stored and
    /// the Job Failed. Nothing in the folder is replaced or deleted: a file the plan removes or writes over is kept
    /// aside in the folder's retention area, and only while it is the file the plan found. A file whose sources fail
    /// their re-hash, or whose output fails validation, is skipped and the rest go on; anything wrong with the folder
    /// itself, its marker included, stops the Build, which then fails without advancing the marker. Returns the Job
    /// as it ends.
    pub fn run_build_job(
        &self,
        id: i64,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<Job> {
        // What it reads and writes is counted for it, even where it isn't handed its meter.
        let _metered = self.meter.job(id).on_this_thread();
        let (kind, state, plan_id) = self.read(|c| {
            c.query_row(
                "SELECT kind, state, plan_id FROM jobs WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("no job {id}")))
        })?;
        if kind != JobKind::Build.as_str() {
            return Err(Error::Refused(format!("job {id} is not a build")));
        }
        if state != JobState::Running.as_str() {
            return Err(Error::Refused(format!("job {id} is {state}, not running")));
        }
        let plan_id = plan_id.ok_or_else(|| Error::Io(format!("build job {id} has no plan")))?;
        if self.cancel_requested(id)? {
            self.write(|tx| {
                finish_running(
                    tx,
                    &self.meter,
                    id,
                    JobState::Interrupted,
                    Some("you cancelled the build before it started; nothing was changed"),
                )
            })?;
            return self.job(id);
        }
        let mut build = None;
        let outcome = self.carry_out(id, plan_id, &mut build, progress);
        if let Err(err) = &outcome {
            let message = err.to_string();
            // The Build is recorded failed, with the Job's error, before its folder is recovered, and the Job ends
            // once recovery has, so that the write ending it records what recovery read and wrote too.
            let recovered = match build {
                Some(build) => {
                    self.write(|tx| fail_build(tx, id, build, &message))?;
                    self.fault_point(Fault::BuildFailed);
                    self.recover_build(build, Some(&self.meter.job(id)))
                        .map(drop)
                }
                None => Ok(()),
            };
            self.write(|tx| end_failed(tx, &self.meter, id, &message))?;
            recovered?;
        }
        // Additions a cleanup kept aside drift entries for are published from their kept files by their
        // Additions Jobs.
        self.write(|tx| crate::additions::queue_additions_jobs(tx).map(drop))?;
        let job = self.job(id)?;
        if job.state == JobState::Succeeded {
            self.retain_blobs()?;
            // A build that removed every built file ends with the folder unregistered, unless something came to
            // hold it meanwhile; then it stays registered, empty, and you unregister it once nothing does.
            let plan = self.plan(plan_id)?;
            // The files of the games it removed to delete are deleted once it has kept them aside; any it couldn't
            // delete stay kept.
            if let Some(build) = self.build_of_job(id)? {
                self.fault_point(Fault::RemovedKept);
                match self.delete_removed(&plan, build.id) {
                    Ok(()) | Err(Error::Conflict(_) | Error::Refused(_)) => {}
                    Err(err) => return Err(err),
                }
            }
            let content = plan.content;
            if content.choices.remove_all {
                match self.unregister(RegistrationKind::LibraryFolder, content.folder) {
                    Ok(_) | Err(Error::Conflict(_)) => {}
                    Err(err) => return Err(err),
                }
            }
        }
        Ok(job)
    }

    fn carry_out(
        &self,
        job: i64,
        plan_id: i64,
        build_id: &mut Option<i64>,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<()> {
        let roots = &*self.roots();
        let plan = self.plan(plan_id)?;
        let approved = plan
            .approval
            .as_ref()
            .ok_or_else(|| Error::Io(format!("plan {plan_id} is not approved")))?;
        let content = &plan.content;
        let again = self.recompute(content)?;
        if digest(&again.content) != approved.digest {
            let newer = self.write(|tx| super::plans::insert_plan(tx, &again, None))?;
            return Err(Error::Conflict(format!(
                "the plan changed since you approved it; nothing was written. Review plan {} instead",
                newer.id
            )));
        }
        if let Some(cleanup) = &content.cleanup {
            return self
                .carry_out_cleanup(job, plan_id, content, cleanup, &again, build_id, progress);
        }
        let audit = content
            .audit
            .ok_or_else(|| Error::Io(format!("plan {plan_id} has no audit")))?;
        let (folder_row, store) =
            self.read(|c| Ok((folder_by_id(c, content.folder)?, store_id(c)?)))?;
        let dir = open_writable_folder(roots, &folder_row.root, &folder_row.path)
            .map_err(Error::Refused)?;
        // What verifying a conversion's file reads back from the Library Folder's Root.
        let library = self.meter.job(job).tally(Some(&folder_row.root));
        let expected = Marker {
            store,
            folder: content.folder,
            generation: content.generation,
        };
        check_marker(&dir, &expected).map_err(Error::Refused)?;
        for leftover in [STAGING, MARKER_NEW] {
            if dir
                .stat(leftover)
                .map_err(|e| Error::Io(e.to_string()))?
                .is_some()
            {
                return Err(Error::Refused(format!(
                    "{} holds {leftover}, which no build of this store left there; nothing was written",
                    dir.path().display()
                )));
            }
        }
        // The Additions Set the plan's Additions are copied from: one, the folder's system's.
        let additions = match content.items.iter().find_map(|i| i.addition.as_ref()) {
            Some(a) => Some(
                self.open_additions(a.additions_set)?
                    .map_err(Error::Refused)?,
            ),
            None => None,
        };
        // The files to write, then the files to adopt, which are only recorded.
        let (writes, adopts): (Vec<&PlanItem>, Vec<&PlanItem>) = content
            .items
            .iter()
            .filter(|i| i.kind.writes() || i.kind == ItemKind::Adopt)
            .partition(|i| i.kind.writes());
        let all: Vec<Work> = writes
            .into_iter()
            .chain(adopts)
            .enumerate()
            .map(|(seq, item)| Work {
                seq: seq as i64,
                item,
            })
            .collect();
        let (work, adopts) = all.split_at(all.iter().take_while(|w| w.item.kind.writes()).count());
        // The Source Set's folder is read only by a build that writes: one that only removes or adopts files, such as
        // removing every built file, runs while it is unavailable, or bound to another Source Set since.
        let set = match work.is_empty() {
            true => None,
            false => {
                let (set_dir, set_root) = self.set_folder(folder_row.source_set_id)?;
                let set_fd = self.set_folder_dir(folder_row.source_set_id)?;
                Some((set_dir, set_root, set_fd))
            }
        };
        // A Build that places Games, or returns them, changes their Source Set's folder: it is opened to write to as a
        // repair opens it.
        let placing = match work.iter().any(|w| w.item.kind == ItemKind::Place)
            || !content.returns.is_empty()
        {
            true => Some(
                self.open_placing(folder_row.source_set_id)?
                    .map_err(|why| {
                        Error::Refused(format!(
                            "source set {} can't be changed to place its games or return them: {why}; nothing was \
                             written",
                            folder_row.source_set
                        ))
                    })?,
            ),
            false => None,
        };
        // The Source Set's folder changes: a repair planned before this Build is refused, as one planned before
        // another repair is. A Source Set file it keeps rather than deletes is kept under the generation it makes.
        let set_generation = match placing.is_some() {
            true => self.write(|tx| {
                Ok(tx.query_row(
                    "UPDATE source_folders SET generation = generation + 1 WHERE source_set_id = ?1
                     RETURNING generation",
                    [folder_row.source_set_id],
                    |r| r.get::<_, i64>(0),
                )? as u64)
            })?,
            false => 0,
        };
        // Then the built files no longer selected: each kept aside, or forgotten when it is gone already.
        let removals: Vec<(i64, &PlanRemoval)> = content
            .removals
            .iter()
            .filter(|r| r.kind != RemovalKind::Leave)
            .enumerate()
            .map(|(i, r)| ((all.len() + i) as i64, r))
            .collect();
        // Then the originals of the conversions: each the file built for a game in the other format, kept aside once
        // its game's new file is in place.
        let conversions: Vec<(i64, &Work, &stage_select_api::build::KeepAside)> = work
            .iter()
            .filter_map(|w| w.item.converts.as_ref().map(|k| (w, k)))
            .enumerate()
            .map(|(i, (w, k))| ((all.len() + removals.len() + i) as i64, w, k))
            .collect();
        // Then the releases placed here that go back to their Source Set's folder.
        let returning: Vec<(i64, &stage_select_api::build::PlanReturn)> = content
            .returns
            .iter()
            .enumerate()
            .map(|(i, r)| {
                (
                    (all.len() + removals.len() + conversions.len() + i) as i64,
                    r,
                )
            })
            .collect();
        let generation = content.generation + 1;
        let delete = content.storage.delete_originals;
        let id = self.write(|tx| {
            let revision: i64 = tx.query_row(
                "SELECT dat_revision_id FROM selections WHERE id = ?1",
                [content.selection],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO builds (folder_id, plan_id, job_id, selection_id, audit_id, dat_revision_id, state,
                                     generation, started_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'running', ?7, ?8)",
                params![
                    content.folder,
                    plan_id,
                    job,
                    content.selection,
                    audit,
                    revision,
                    generation as i64,
                    now()
                ],
            )?;
            let id = tx.last_insert_rowid();
            for w in &all {
                tx.execute(
                    "INSERT INTO build_items (build_id, seq, target, game, kind, state)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
                    params![id, w.seq, w.item.target, w.item.game, w.item.kind.as_str()],
                )?;
            }
            for (seq, r) in &removals {
                tx.execute(
                    "INSERT INTO build_items (build_id, seq, target, game, kind, state)
                     VALUES (?1, ?2, ?3, ?4, 'remove', 'pending')",
                    params![id, seq, r.target, r.game],
                )?;
            }
            for (seq, w, k) in &conversions {
                tx.execute(
                    "INSERT INTO build_items (build_id, seq, target, game, kind, state)
                     VALUES (?1, ?2, ?3, ?4, 'remove', 'pending')",
                    params![id, seq, k.name, w.item.game],
                )?;
            }
            for (seq, r) in &returning {
                tx.execute(
                    "INSERT INTO build_items (build_id, seq, target, game, kind, state)
                     VALUES (?1, ?2, ?3, ?4, 'return', 'pending')",
                    params![id, seq, r.release, r.games.join(", ")],
                )?;
            }
            Ok(id)
        })?;
        *build_id = Some(id);

        let folder_io = |e: io::Error| Error::Io(e.to_string());
        let folder = content.folder;
        let staging = create_dir(self, folder, &dir, STAGING, STAGING)?.map_err(Error::Refused)?;
        let staging_id = staging.identity().map_err(folder_io)?;
        let mut staged_ids = HashMap::new();
        let built_at = SystemTime::now();
        let total = all.len() + removals.len() + conversions.len() + returning.len();
        // The retention area is created the first time a file is kept aside.
        let mut retention: Option<Retention> = None;
        let open_area = |retention: &mut Option<Retention>| -> Result<()> {
            if retention.is_none() {
                *retention = Some(
                    open_retention(self, folder, &dir, generation, true)?
                        .map_err(Error::Refused)?
                        .ok_or_else(|| Error::Io("the retention area was not created".into()))?,
                );
            }
            Ok(())
        };
        if let Some(progress) = progress {
            progress(Progress {
                files_done: 0,
                files_total: total,
            });
        }
        let mut done = 0;
        let tick = |done: &mut usize| {
            *done += 1;
            if let Some(progress) = progress {
                progress(Progress {
                    files_done: *done,
                    files_total: total,
                });
            }
        };
        // An original whose game's file is already in place in the format the storage policy says, left by a
        // conversion that stopped before it was disposed of.
        let survivor = |removal: &PlanRemoval| {
            removal.converts_to.as_ref().and_then(|target| {
                content
                    .items
                    .iter()
                    .find(|i| i.target == *target && i.kind == ItemKind::Unchanged)
            })
        };
        let remove = |seq: i64,
                      removal: &PlanRemoval,
                      retention: &mut Option<Retention>|
         -> Result<()> {
            check_marker(&dir, &expected).map_err(Error::Refused)?;
            // A file its release's new files replace is kept aside only once every one of them is in place: the
            // release's whole unit committed and finished. If it rolled back, the file stays.
            let release = stage_select_api::storage::release_of(&removal.game);
            let mut replacing = None;
            for w in all.iter().filter(|w| {
                w.item.kind.writes()
                    && w.item.addition.is_none()
                    && stage_select_api::storage::release_of(&w.item.game) == release
            }) {
                if item_state(self, id, w.seq)?.as_deref() != Some("applied") {
                    replacing = Some(w);
                    break;
                }
            }
            let outcome = match (&removal.kind, &removal.keep) {
                (RemovalKind::Keep, Some(keep)) if survivor(removal).is_some() => {
                    open_area(retention)?;
                    let item = survivor(removal).expect("matched above");
                    let original = self.read(|c| conversion_record(c, folder, keep, &item.target))?;
                    let k = Keep {
                        folder,
                        build: id,
                        seq,
                        generation,
                        keep,
                        hashed: None,
                        original,
                    };
                    let area = retention.as_ref().expect("opened above");
                    record_disposal(
                        self,
                        id,
                        seq,
                        self.dispose_original((&dir, &library), area, &k, item, delete, &approved.actor)?,
                    )?;
                    Ok(())
                }
                (RemovalKind::Keep, Some(_)) if replacing.is_some() => {
                    Err(format!(
                        "{} was not written, so {} stays as it is",
                        replacing.expect("matched").item.target,
                        removal.target
                    ))
                }
                (RemovalKind::Keep, Some(keep)) => {
                    open_area(retention)?;
                    let k = Keep {
                        folder,
                        build: id,
                        seq,
                        generation,
                        keep,
                        hashed: None,
                        original: None,
                    };
                    let area = retention.as_ref().expect("opened above");
                    self.keep_aside((&dir, &library), area, &k)?.map(|_| ())
                }
                (RemovalKind::Forget, _) => match dir.stat(&removal.target).map_err(folder_io)? {
                    None => self.write(|tx| {
                        tx.execute(
                            "DELETE FROM built_files WHERE folder_id = ?1 AND target = ?2",
                            params![folder, removal.target],
                        )?;
                        tx.execute(
                            "DELETE FROM conversion_originals WHERE folder_id = ?1 AND target = ?2",
                            params![folder, removal.target],
                        )?;
                        tx.execute(
                            "UPDATE build_items SET state = 'applied' WHERE build_id = ?1 AND seq = ?2",
                            params![id, seq],
                        )?;
                        Ok(Ok(()))
                    })?,
                    Some(_) => Err(format!(
                        "{} is back in the folder since you approved the plan; it was left as it is",
                        removal.target
                    )),
                },
                _ => Err(format!("{} has nothing to remove", removal.target)),
            };
            if let Err(error) = outcome {
                self.write(|tx| set_item(tx, id, seq, ItemState::Failed, &error))?;
            }
            Ok(())
        };
        let cancel_asked = || self.cancel_requested(job).unwrap_or(false);
        let limits = self.settings()?.chd_limits();
        let threads = self.chdman_threads()?;
        let meter = self.meter.job(job);
        let placed_chds: HashMap<String, crate::placements::PlacedChd> = self
            .read(|c| crate::placements::placed_chds_of(c, folder_row.source_set_id))?
            .into_iter()
            .map(|p| (stage_select_api::storage::placed_path(p.folder, &p.path), p))
            .collect();
        let chd_work = ChdWork {
            // A CHD is made or verified with chdman, a return extracts any CHD it takes home, and an item read from a
            // placed CHD reads it so.
            chdman: match work.iter().any(|w| {
                w.item.chd
                    || w.item
                        .sources
                        .iter()
                        .any(|s| placed_chds.contains_key(&s.path))
            }) || !returning.is_empty()
            {
                true => self.chdman().map(|program| Chdman {
                    program,
                    threads,
                    deadline: limits.base,
                    tally: meter.tally(None),
                }),
                false => Err("no item is a CHD, and nothing is returned".into()),
            },
            store: self,
            to: meter.tally(Some(&folder_row.root)),
            from: match &set {
                Some(_) => meter.tally(Some(
                    &self
                        .read(|c| crate::sets::set_by_id(c, folder_row.source_set_id))?
                        .record
                        .root,
                )),
                // With no Source Set, nothing is read from one.
                None => Tally::default(),
            },
            additions: meter.tally(self.additions_root_name().as_deref()),
            scratch: meter.tally(None),
            set: folder_row.source_set_id,
            placed_chds: placed_chds.clone(),
            limits,
            tmp: self.dir().join(crate::TMP),
            build: id,
            cancelled: &cancel_asked,
            placed: {
                let mut folders: Vec<i64> = work
                    .iter()
                    .flat_map(|w| &w.item.sources)
                    .filter_map(|s| {
                        stage_select_api::storage::split_placed(&s.path).map(|(f, _)| f)
                    })
                    .collect();
                folders.sort_unstable();
                folders.dedup();
                let mut placed = HashMap::new();
                for f in folders {
                    let row = self.read(|c| folder_by_id(c, f))?;
                    placed.insert(f, super::folders::open_folder(roots, &row.root, &row.path));
                }
                placed
            },
        };
        if chd_work.chdman.is_ok() {
            std::fs::create_dir_all(&chd_work.tmp).map_err(|e| Error::Io(e.to_string()))?;
        }
        let mut cancelled = false;
        // The originals earlier conversions left are disposed of first, before any new file is written.
        for (seq, removal) in removals.iter().filter(|(_, r)| survivor(r).is_some()) {
            if self.cancel_requested(job)? {
                cancelled = true;
                break;
            }
            remove(*seq, removal, &mut retention)?;
            tick(&mut done);
        }
        // Each game is done before the next starts: its files written, put in place and recorded, and the originals
        // they convert kept aside, or deleted. The discs of one release that change format, or become CHDs, are one
        // unit: every disc is written and verified before any is put in place, and once one is renamed into place
        // the rest follow, whether the Build is cancelled or stops (recovery finishes them). A cancelled Build stops
        // between two units, or before a unit's first rename.
        let mut cleared: HashSet<i64> = HashSet::new();
        let mut units: Vec<Vec<&Work>> = Vec::new();
        let mut releases: HashMap<(String, bool), usize> = HashMap::new();
        for w in work {
            // Every file of a release goes in place with the rest of it, whatever its format and layout: each disc,
            // each track, and the files generated beside them. An addition goes alone. The files of a release placed
            // under Single Copy are placed together, apart from those written fresh beside them.
            if w.item.addition.is_none() {
                let release = (
                    stage_select_api::storage::release_of(&w.item.game),
                    w.item.kind == ItemKind::Place,
                );
                if let Some(&u) = releases.get(&release) {
                    units[u].push(w);
                    continue;
                }
                releases.insert(release, units.len());
            }
            units.push(vec![w]);
        }
        // A release placed goes in place right after the files written fresh beside it, and only once they are in
        // place: a Game's Disk is copied and verified before its ZIP is placed, and a cancel doesn't come between them.
        // No other release reads a file a release placed consumes (the plan places none another reads). A file
        // written fresh is a copy: one in place stays when its release's placement then fails, and the folder lacks
        // the release.
        // Each release's units sort by where its first one was, its files written fresh before its files placed.
        let mut first: HashMap<String, usize> = HashMap::new();
        for (i, u) in units.iter().enumerate() {
            if u[0].item.addition.is_none() {
                first
                    .entry(stage_select_api::storage::release_of(&u[0].item.game))
                    .or_insert(i);
            }
        }
        let mut keyed: Vec<((usize, bool), Vec<&Work>)> = units
            .into_iter()
            .enumerate()
            .map(|(i, u)| {
                let at = match u[0].item.addition {
                    None => first[&stage_select_api::storage::release_of(&u[0].item.game)],
                    Some(_) => i,
                };
                ((at, u[0].item.kind == ItemKind::Place), u)
            })
            .collect();
        keyed.sort_by_key(|(k, _)| *k);
        let units: Vec<Vec<&Work>> = keyed.into_iter().map(|(_, u)| u).collect();
        /// A file of a unit, written and verified in the staging directory.
        struct Ready<'w, 'a> {
            w: &'w Work<'a>,
            name: String,
            file: File,
            file_id: Identity,
            staged: Staged,
            staged_path: PathBuf,
        }
        let disc_system = stage_select_api::storage::is_disc_system(&folder_row.system);
        let placer = place::Placing {
            build: id,
            folder,
            set: folder_row.source_set_id,
            set_name: &folder_row.source_set,
            audit,
            dir: &dir,
            expected: &expected,
            roots,
            root: &folder_row.root,
            path: &folder_row.path,
            generation: set_generation,
            to: &chd_work.to,
            from: &chd_work.from,
        };
        // The release whose files written fresh were last put in place: its files placed follow them, even once the
        // Operator cancels.
        let mut beside: Option<String> = None;
        for unit in &units {
            if cancelled {
                break;
            }
            let release = stage_select_api::storage::release_of(&unit[0].item.game);
            let follows = unit[0].item.kind == ItemKind::Place && beside.as_ref() == Some(&release);
            beside = (unit[0].item.kind != ItemKind::Place && unit[0].item.addition.is_none())
                .then(|| release.clone());
            if !follows && self.cancel_requested(job)? {
                cancelled = true;
                break;
            }
            // A release placed is placed whole, or not at all.
            if let Some(set) = &placing
                && unit[0].item.kind == ItemKind::Place
            {
                let mut culprit = None;
                for w in work.iter().filter(|w| {
                    w.item.kind != ItemKind::Place
                        && w.item.addition.is_none()
                        && stage_select_api::storage::release_of(&w.item.game) == release
                }) {
                    if item_state(self, id, w.seq)?.as_deref() != Some("applied") {
                        culprit = Some(&w.item.target);
                        break;
                    }
                }
                if let Some(culprit) = culprit {
                    self.fail_unit(
                        &placer,
                        unit,
                        &format!(
                            "{} {} not put in place, because {culprit} of the same game wasn't",
                            unit.iter()
                                .map(|w| w.item.target.as_str())
                                .collect::<Vec<_>>()
                                .join(" and "),
                            if unit.len() == 1 { "was" } else { "were" }
                        ),
                    )?;
                } else {
                    self.place_unit(
                        &placer,
                        set,
                        unit,
                        (&staging, &mut staged_ids, built_at),
                        &chd_work,
                    )?;
                }
                for _ in unit {
                    tick(&mut done);
                }
                continue;
            }
            // A release of several files, or any disc release, which may clear its other layout or keep an original
            // aside before it goes in place, commits durably: recovery then finishes it rather than drops it.
            let shared = (unit.len() > 1 || (disc_system && unit[0].item.addition.is_none()))
                .then(|| stage_select_api::storage::release_of(&unit[0].item.game));
            if let Some(release) = &shared {
                self.write(|tx| {
                    tx.execute(
                        "INSERT INTO build_units (build_id, unit, state) VALUES (?1, ?2, 'staged')",
                        params![id, release],
                    )?;
                    for w in unit {
                        tx.execute(
                            "UPDATE build_items SET unit = ?3 WHERE build_id = ?1 AND seq = ?2",
                            params![id, w.seq, release],
                        )?;
                    }
                    Ok(())
                })?;
            }
            // The original a conversion replaces is read and recorded before anything is written for it.
            let mut originals = HashMap::new();
            let mut ready: Vec<Ready> = Vec::new();
            // The item that failed, so that no file of its unit is put in place, and why.
            let mut failed: Option<(i64, String)> = None;
            for (i, &w) in unit.iter().enumerate() {
                if i > 0 && self.cancel_requested(job)? {
                    cancelled = true;
                    break;
                }
                let name = format!("{}.zip", w.seq);
                let target = &w.item.target;
                if let Some(keep) = &w.item.converts {
                    check_marker(&dir, &expected).map_err(Error::Refused)?;
                    // Reading the original changes nothing, so a cancel stops it as it stops staging: the unit is
                    // then left as one the Build never got to.
                    let interruptible = Interruptible::new();
                    let recorded =
                        record_original(self, (&dir, &library), folder, id, keep, target)?;
                    let interrupted = interruptible.interrupted();
                    drop(interruptible);
                    match recorded {
                        Err(_) if interrupted => {
                            cancelled = true;
                            break;
                        }
                        Ok(read) => {
                            originals.insert(w.seq, read);
                        }
                        Err(error) => {
                            failed = Some((w.seq, error));
                            break;
                        }
                    }
                }
                let created = staging.create_new(&name).and_then(|file| {
                    let id = Identity::of_fd(&file)?;
                    Ok((file, id))
                });
                let (mut file, file_id) = match created {
                    Ok(created) => created,
                    Err(e) => {
                        self.write(|tx| {
                            set_item(tx, id, w.seq, ItemState::Failed, &e.to_string())
                        })?;
                        return Err(Error::Io(e.to_string()));
                    }
                };
                record_temp(self, folder, &staged_temp(&name), file_id)?;
                staged_ids.insert(name.clone(), file_id);
                let staged_path = staging.path().join(&name);
                // Staging is dropped whole on a cancel, so a cancel stops its capped reads and writes at once rather
                // than wait out the caps: whatever failed for it is the cancel.
                let interruptible = Interruptible::new();
                let staged = stage(
                    (w.item, w.seq),
                    &mut file,
                    &staged_path,
                    {
                        let (set_dir, set_root, set_fd) = set
                            .as_ref()
                            .expect("a build that writes reads its source set");
                        (set_dir.as_path(), set_root.as_path(), set_fd)
                    },
                    additions.as_ref(),
                    &chd_work,
                    built_at,
                );
                let staged = match staged {
                    Err(Failure::Item(_) | Failure::Folder(_)) if interruptible.interrupted() => {
                        Err(Failure::Cancelled)
                    }
                    staged => staged,
                };
                drop(interruptible);
                match staged {
                    Err(Failure::Cancelled) => {
                        // Nothing of the unit is put in place: its items are left pending, as ones the Build never
                        // got to.
                        drop(file);
                        remove_staged(&staging, &name, file_id).map_err(Error::Refused)?;
                        cancelled = true;
                        break;
                    }
                    Err(Failure::Item(error)) => {
                        drop(file);
                        if let Err(e) = remove_staged(&staging, &name, file_id) {
                            self.write(|tx| set_item(tx, id, w.seq, ItemState::Failed, &error))?;
                            return Err(Error::Refused(e));
                        }
                        failed = Some((w.seq, error));
                        break;
                    }
                    Err(Failure::Folder(error)) => {
                        self.write(|tx| set_item(tx, id, w.seq, ItemState::Failed, &error))?;
                        return Err(Error::Io(error));
                    }
                    Ok(staged) => {
                        ready.push(Ready {
                            w,
                            name,
                            file,
                            file_id,
                            staged,
                            staged_path,
                        });
                        self.fault_point(Fault::Staged(w.seq));
                    }
                }
            }
            // A unit is committed, with every disc's publish intent, in one transaction that first checks you have
            // not cancelled: before it nothing is kept aside or renamed, and the unit is rolled back; after it every
            // disc goes in place, here or by recovery. A file alone is checked for a cancel the same way, then kept
            // aside, if it must be, before its intent is recorded.
            let intents = |tx: &Transaction, ready: &[Ready]| -> Result<()> {
                for r in ready {
                    let (dev, ino) = r.file_id.to_sql();
                    tx.execute(
                        "UPDATE build_items SET state = 'publishing', sha256 = ?3, size = ?4, dev = ?5, ino = ?6,
                                                chd_sha1 = ?7, chd_raw_sha1 = ?8, mtime_ns = ?9
                         WHERE build_id = ?1 AND seq = ?2",
                        params![
                            id,
                            r.w.seq,
                            r.staged.sha256,
                            r.staged.size as i64,
                            dev,
                            ino,
                            r.staged.chd_sha1,
                            r.staged.chd_raw_sha1,
                            r.staged.mtime_ns as i64
                        ],
                    )?;
                }
                Ok(())
            };
            if !cancelled && failed.is_none() {
                match &shared {
                    Some(release) => {
                        let committed = self.write(|tx| {
                            let asked: bool = tx.query_row(
                                "SELECT cancel_requested_at IS NOT NULL FROM jobs WHERE id = ?1",
                                [job],
                                |r| r.get(0),
                            )?;
                            if asked {
                                return Ok(false);
                            }
                            tx.execute(
                                "UPDATE build_units SET state = 'committing' WHERE build_id = ?1 AND unit = ?2",
                                params![id, release],
                            )?;
                            intents(tx, &ready)?;
                            Ok(true)
                        })?;
                        cancelled = !committed;
                    }
                    None if self.cancel_requested(job)? => cancelled = true,
                    None => {
                        for r in &ready {
                            let Some(keep) = &r.w.item.keep else {
                                continue;
                            };
                            check_marker(&dir, &expected).map_err(Error::Refused)?;
                            open_area(&mut retention)?;
                            let k = Keep {
                                folder,
                                build: id,
                                seq: r.w.seq,
                                generation,
                                keep,
                                hashed: None,
                                original: None,
                            };
                            let area = retention.as_ref().expect("opened above");
                            if let Err(error) = self.keep_aside((&dir, &library), area, &k)? {
                                failed = Some((r.w.seq, error));
                                break;
                            }
                        }
                        if failed.is_none() {
                            self.write(|tx| intents(tx, &ready))?;
                        }
                    }
                }
            }
            if cancelled || failed.is_some() {
                if let Some((seq, error)) = &failed {
                    let culprit = &unit
                        .iter()
                        .find(|w| w.seq == *seq)
                        .expect("in its unit")
                        .item
                        .target;
                    self.write(|tx| {
                        for w in unit {
                            match w.seq == *seq {
                                true => set_item(tx, id, w.seq, ItemState::Failed, error)?,
                                false => set_item(
                                    tx,
                                    id,
                                    w.seq,
                                    ItemState::Failed,
                                    &format!(
                                        "{} was not put in place, because {culprit} of the same game failed; the \
                                         game stays as it was",
                                        w.item.target
                                    ),
                                )?,
                            }
                        }
                        Ok(())
                    })?;
                }
                for r in ready.drain(..) {
                    drop(r.file);
                    remove_staged(&staging, &r.name, r.file_id).map_err(Error::Refused)?;
                }
                if let Some(release) = &shared {
                    self.write(|tx| {
                        tx.execute(
                            "UPDATE build_units SET state = 'rolled_back' WHERE build_id = ?1 AND unit = ?2",
                            params![id, release],
                        )?;
                        Ok(())
                    })?;
                }
                if cancelled {
                    break;
                }
            } else {
                // A committed unit that can't be finished here stops the Build, its staged files kept: recovery
                // finishes it, or holds the folder until it can.
                let stuck = |target: &str, error: &str| {
                    Error::Refused(unit_stuck(
                        shared.as_deref().expect("a unit"),
                        target,
                        error,
                    ))
                };
                if shared.is_some() {
                    for r in &ready {
                        let Some(keep) = &r.w.item.keep else {
                            continue;
                        };
                        check_marker(&dir, &expected).map_err(Error::Refused)?;
                        open_area(&mut retention)?;
                        let k = Keep {
                            folder,
                            build: id,
                            seq: r.w.seq,
                            generation,
                            keep,
                            hashed: None,
                            original: None,
                        };
                        let area = retention.as_ref().expect("opened above");
                        if let Err(error) = self.keep_aside((&dir, &library), area, &k)? {
                            return Err(stuck(&r.w.item.target, &error));
                        }
                    }
                }
                for r in ready.drain(..) {
                    let (w, name, target) = (r.w, &r.name, &r.w.item.target);
                    if content
                        .removals
                        .iter()
                        .any(|x| super::plans::clears(&x.target, target))
                    {
                        check_marker(&dir, &expected).map_err(Error::Refused)?;
                        open_area(&mut retention)?;
                        let area = retention.as_ref().expect("opened above");
                        match self.clear_way(
                            (&dir, &library),
                            area,
                            (folder, id, generation),
                            &content.removals,
                            target,
                        )? {
                            Ok(seqs) => cleared.extend(seqs),
                            Err(error) if shared.is_some() => return Err(stuck(target, &error)),
                            Err(error) => {
                                self.write(|tx| {
                                    set_item(tx, id, w.seq, ItemState::Failed, &error)
                                })?;
                                return Err(Error::Refused(error));
                            }
                        }
                    }
                    self.fault_point(Fault::BeforePublish(w.seq));
                    // The file renamed is the one verified: the staged name must still be it, and the target must be
                    // it once renamed. The verified file stays open until then.
                    let renamed = check_marker(&dir, &expected)
                        .and_then(|()| match is_file(&staging, name, r.file_id)? {
                            true => Ok(()),
                            false => Err(format!(
                                "{} was replaced after it was verified; it was left as it is",
                                r.staged_path.display()
                            )),
                        })
                        .and_then(|()| make_parents(&dir, target).map_err(|e| e.to_string()))
                        .and_then(|made| {
                            record_layout_dir(self, folder, made).map_err(|e| e.to_string())
                        })
                        .and_then(|()| {
                            staging
                                .rename_new(name, &dir, target)
                                .map_err(|e| match e.kind() {
                                    io::ErrorKind::AlreadyExists => format!(
                                        "{target} appeared in the folder during the build; it was left as it is"
                                    ),
                                    _ => e.to_string(),
                                })
                        });
                    if let Err(error) = renamed {
                        if shared.is_some() {
                            return Err(stuck(target, &error));
                        }
                        self.write(|tx| set_item(tx, id, w.seq, ItemState::Failed, &error))?;
                        return Err(Error::Refused(error));
                    }
                    // From here until the item is recorded applied, recovery settles it from its publish intent.
                    self.fault_point(Fault::PublishedUnsynced(w.seq));
                    staging
                        .sync()
                        .and_then(|()| dir.sync())
                        .and_then(|()| sync_parent(&dir, target))
                        .map_err(folder_io)?;
                    self.fault_point(Fault::ParentSynced(w.seq));
                    self.fault_point(Fault::AfterPublish(w.seq));
                    let found = dir.stat(target).map_err(folder_io)?;
                    let applied = match &found {
                        Some(st) => self
                            .write(|tx| record_applied(tx, id, folder, w.seq, w.item, st, true))?,
                        None => {
                            self.write(|tx| {
                                set_item(
                                    tx,
                                    id,
                                    w.seq,
                                    ItemState::Uncertain,
                                    &format!("{target} vanished once it was put in place"),
                                )
                            })?;
                            false
                        }
                    };
                    if !applied {
                        return Err(Error::Refused(format!(
                            "{}: {target} is not the file the build put there; it was left as it is",
                            dir.path().display()
                        )));
                    }
                    drop(r.file);
                }
                if let Some(release) = &shared {
                    self.write(|tx| {
                        tx.execute(
                            "UPDATE build_units SET state = 'done' WHERE build_id = ?1 AND unit = ?2",
                            params![id, release],
                        )?;
                        Ok(())
                    })?;
                }
            }
            for w in unit {
                tick(&mut done);
                let Some((seq, _, keep)) = conversions.iter().find(|(_, c, _)| c.seq == w.seq)
                else {
                    continue;
                };
                check_marker(&dir, &expected).map_err(Error::Refused)?;
                let outcome = if item_state(self, id, w.seq)?.as_deref() != Some("applied") {
                    Err(format!(
                        "{} was not written, so {} stays as it is",
                        w.item.target, keep.name
                    ))
                } else {
                    open_area(&mut retention)?;
                    let k = Keep {
                        folder,
                        build: id,
                        seq: *seq,
                        generation,
                        keep,
                        hashed: None,
                        original: originals.remove(&w.seq),
                    };
                    let area = retention.as_ref().expect("opened above");
                    self.dispose_original(
                        (&dir, &library),
                        area,
                        &k,
                        w.item,
                        delete,
                        &approved.actor,
                    )?
                };
                record_disposal(self, id, *seq, outcome)?;
                tick(&mut done);
            }
        }
        // The releases placed here that go home go last, each whole or not at all, once every file the plan reads from
        // their placed files is written.
        if let Some(set) = &placing
            && !returning.is_empty()
        {
            let set_root = self
                .read(|c| crate::sets::set_by_id(c, folder_row.source_set_id))?
                .record
                .root;
            let to = meter.tally(Some(&set_root));
            let returner = returns::Returning {
                build: id,
                folder,
                set: folder_row.source_set_id,
                set_name: &folder_row.source_set,
                dir: &dir,
                roots,
                to: &to,
                from: &library,
                chd: Some(&chd_work),
            };
            for (seq, r) in &returning {
                if cancelled || self.cancel_requested(job)? {
                    cancelled = true;
                    break;
                }
                check_marker(&dir, &expected).map_err(Error::Refused)?;
                self.return_release(&returner, set, *seq, r, built_at)?;
                tick(&mut done);
            }
        }
        clear_staging(&dir, &staging, staging_id, &staged_ids).map_err(Error::Refused)?;
        drop(staging);
        for w in adopts {
            if cancelled || self.cancel_requested(job)? {
                cancelled = true;
                break;
            }
            check_marker(&dir, &expected).map_err(Error::Refused)?;
            match adopt((&dir, &library), w.item) {
                Ok((sha256, st, torrentzip)) => {
                    let (dev, ino) = Identity::of(&st).to_sql();
                    let parent = adopted_parent(&dir, &w.item.target).map_err(folder_io)?;
                    self.write(|tx| {
                        tx.execute(
                            "UPDATE build_items SET sha256 = ?3, size = ?4, dev = ?5, ino = ?6
                             WHERE build_id = ?1 AND seq = ?2",
                            params![id, w.seq, sha256, st.st_size, dev, ino],
                        )?;
                        if record_applied(tx, id, folder, w.seq, w.item, &st, torrentzip)? {
                            record_adopted_parent(tx, folder, parent.as_ref())?;
                        }
                        Ok(())
                    })?;
                }
                Err(error) => {
                    self.write(|tx| set_item(tx, id, w.seq, ItemState::Failed, &error))?
                }
            }
            tick(&mut done);
        }
        for _ in &cleared {
            tick(&mut done);
        }
        for (seq, removal) in removals
            .iter()
            .filter(|(seq, r)| survivor(r).is_none() && !cleared.contains(seq))
        {
            if cancelled || self.cancel_requested(job)? {
                cancelled = true;
                break;
            }
            remove(*seq, removal, &mut retention)?;
            tick(&mut done);
        }
        // A disc layout folder a Build made, whose every file was kept aside, goes too.
        for target in content
            .removals
            .iter()
            .map(|r| &r.target)
            .chain(
                content
                    .items
                    .iter()
                    .filter_map(|i| i.keep.as_ref().map(|k| &k.name)),
            )
            .chain(conversions.iter().map(|(_, _, k)| &k.name))
        {
            remove_layout_dir(self, &dir, folder, target)?;
        }
        drop(retention);
        if cancelled {
            // What is in place stays, recorded as built; the folder stays at its generation, and a new plan
            // finishes it.
            return self.write(|tx| end_cancelled(tx, &self.meter, job, id, folder));
        }

        // The store decides the next generation first, and the marker follows it; recovery moves a marker that did
        // not follow forward, and never back.
        check_marker(&dir, &expected).map_err(Error::Refused)?;
        let key = cited_by(folder);
        self.write(|tx| {
            let advanced = tx.execute(
                "UPDATE library_folders SET generation = ?2, current_build_id = ?3 WHERE id = ?1 AND generation = ?4",
                params![folder, generation as i64, id, content.generation as i64],
            )?;
            if advanced != 1 {
                return Err(Error::Io(format!(
                    "library folder {folder} is no longer at generation {}",
                    content.generation
                )));
            }
            tx.execute(
                "UPDATE builds SET finalizing_at = ?2 WHERE id = ?1",
                params![id, now()],
            )?;
            record_incomplete(tx, id, &content.items, &again.retained)?;
            tx.execute("DELETE FROM audit_citations WHERE cited_by = ?1", [&key])?;
            tx.execute(
                "INSERT INTO audit_citations (audit_id, cited_by) VALUES (?1, ?2)",
                params![audit, key],
            )?;
            tx.execute(
                "DELETE FROM selection_citations WHERE cited_by = ?1",
                [&key],
            )?;
            tx.execute(
                "INSERT INTO selection_citations (selection_id, cited_by) VALUES (?1, ?2)",
                params![content.selection, key],
            )?;
            Ok(())
        })?;
        self.fault_point(Fault::AfterFinalize);
        write_marker(
            &dir,
            &Marker {
                generation,
                ..expected.clone()
            },
            &mut |tmp| record_temp(self, folder, MARKER_NEW, tmp),
        )?;
        self.fault_point(Fault::AfterMarker);
        self.write(|tx| {
            tx.execute(
                "UPDATE builds SET state = 'succeeded', finished_at = ?2, cleaned_at = ?2 WHERE id = ?1",
                params![id, now()],
            )?;
            tx.execute("DELETE FROM folder_temps WHERE folder_id = ?1", [folder])?;
            finish_running(tx, &self.meter, job, JobState::Succeeded, None)
        })
    }

    /// Keeps aside, as item `k.seq` of its Build, the original a conversion replaced with `item`'s file, now in place.
    /// With `delete`, it is then deleted from the kept files, but only once `item`'s file verifies, and only while
    /// the original, read back, is still what the store recorded of it. Returns what became of it, or why it was
    /// left where it is.
    fn dispose_original(
        &self,
        (dir, tally): (&FolderDir, &Tally),
        area: &Retention,
        k: &Keep,
        item: &PlanItem,
        delete: bool,
        actor: &str,
    ) -> Result<std::result::Result<Option<String>, String>> {
        let kept = match self.keep_aside((dir, tally), area, k)? {
            Ok(kept) => kept,
            Err(why) => return Ok(Err(why)),
        };
        self.fault_point(Fault::ConversionKept(k.seq));
        if !delete {
            return Ok(Ok(None));
        }
        let name = &k.keep.name;
        if k.original.is_none() {
            return Ok(Ok(Some(format!(
                "{name} stays in the kept files: the conversion has no record of it as it read it"
            ))));
        }
        // The CHD as the Build that wrote it verified it: its header's SHA-1, and the stamp it recorded once the file
        // was written, with its SHA-256, before it was put in place.
        let chd: Option<Verified> = match item.chd {
            true => self.read(|c| {
                Ok(c.query_row(
                    "SELECT f.chd_sha1, i.dev, i.ino, i.size, i.mtime_ns, f.sha256
                     FROM built_files f
                     JOIN build_items i ON i.build_id = f.build_id AND i.target = f.target AND i.sha256 = f.sha256
                     WHERE f.folder_id = ?1 AND f.target = ?2 AND f.chd_sha1 IS NOT NULL AND i.dev IS NOT NULL
                       AND i.ino IS NOT NULL AND i.size IS NOT NULL AND i.mtime_ns IS NOT NULL",
                    params![k.folder, item.target],
                    |r| {
                        Ok(Verified {
                            sha1: r.get(0)?,
                            sha256: r.get(5)?,
                            identity: Identity::from_sql(r.get(1)?, r.get(2)?),
                            file: FileStat {
                                size: r.get::<_, i64>(3)? as u64,
                                mtime_ns: r.get::<_, i64>(4)? as i128,
                            },
                        })
                    },
                )
                .optional()?)
            })?,
            false => None,
        };
        let note = match converted_verified(dir, item, chd.as_ref(), tally)? {
            Err(why) => format!("{name} stays in the kept files: {why}"),
            Ok(()) => match self.delete_converted_kept((dir, tally), kept, actor)? {
                outcome if outcome.deleted => {
                    format!("{name} was deleted once {} was verified", item.target)
                }
                outcome => format!(
                    "{name} stays in the kept files: {}",
                    outcome.detail.unwrap_or_default()
                ),
            },
        };
        Ok(Ok(Some(note)))
    }

    /// Cleans up the folder of the unfinished Build `build`: settles each item it was renaming into place, removes
    /// its staging directory and a half-written marker, and puts back a marker it advanced. Nothing is changed unless
    /// the folder's marker shows it is the store's folder, at the store's generation or, with the Build's finalize
    /// intent recorded, at the Build's. Returns why the folder was left as it is, if it was; the Build then still
    /// needs recovery, and plans for its folder are blocked.
    /// What it reads back from the Library Folder's Root is counted to `job`, the Build's Job, if it is still running.
    fn recover_build(&self, build: i64, job: Option<&JobMeter>) -> Result<Option<String>> {
        // A CHD's work space, left by a Build that stopped while chdman ran, holds nothing it needs.
        remove_chd_dirs(&self.dir().join(crate::TMP), build);
        let (unfinished, folder, store) = self.read(|c| {
            let (folder, plan, generation, finalizing): (i64, i64, i64, bool) = c.query_row(
                "SELECT folder_id, plan_id, generation, finalizing_at IS NOT NULL FROM builds WHERE id = ?1",
                [build],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let mut stmt = c.prepare(
                "SELECT seq, target, sha256, size, kind, origin, dev, ino, kept_id, unit FROM build_items
                 WHERE build_id = ?1
                   AND (state = 'publishing'
                        OR (state <> 'applied'
                            AND unit IN (SELECT unit FROM build_units WHERE build_id = ?1 AND state = 'committing')))
                 ORDER BY seq",
            )?;
            let publishing = stmt
                .query_map([build], |r| {
                    Ok(Publishing {
                        seq: r.get(0)?,
                        target: r.get(1)?,
                        sha256: r.get(2)?,
                        size: r.get(3)?,
                        kind: r.get(4)?,
                        origin: r.get(5)?,
                        dev: r.get(6)?,
                        ino: r.get(7)?,
                        kept_id: r.get(8)?,
                        unit: r.get(9)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            Ok((
                Unfinished {
                    plan,
                    generation: generation as u64,
                    finalizing,
                    publishing,
                },
                folder_by_id(c, folder)?,
                store_id(c)?,
            ))
        })?;
        let library = job
            .map(|job| job.tally(Some(&folder.root)))
            .unwrap_or_default();
        let outcome =
            match self.clean_folder((build, &library), &unfinished, &folder, store.clone())? {
                // Then each Game it was placing, or returning, is rolled forward or back, through the folder at its
                // generation now.
                Ok(()) => {
                    match self.recover_placing(build, unfinished.plan, &folder, store.clone())? {
                        Ok(()) => self.recover_returning(build, unfinished.plan, &folder, store)?,
                        Err(why) => Err(why),
                    }
                }
                Err(why) => Err(why),
            };
        let error = outcome.as_ref().err().cloned();
        self.write(|tx| {
            tx.execute(
                "UPDATE builds SET cleaned_at = CASE WHEN ?2 IS NULL THEN ?3 END, recovery_error = ?2 WHERE id = ?1",
                params![build, error, now()],
            )?;
            if error.is_none() {
                tx.execute(
                    "DELETE FROM folder_temps WHERE folder_id = ?1",
                    [folder.id],
                )?;
            }
            Ok(())
        })?;
        Ok(error)
    }

    /// The files of the release a Build kept aside to clear the way for `target`, in their other disc layout: at
    /// `target`, or in a folder where `target`'s folder goes. Each is kept aside, once its unit committed, before
    /// `target` is renamed into place, and an emptied layout folder a Build made goes with them. The rest of the
    /// release's originals wait until the whole unit is in place.
    fn clear_way(
        &self,
        (dir, library): (&FolderDir, &Tally),
        retention: &Retention,
        (folder, build, generation): (i64, i64, u64),
        removals: &[PlanRemoval],
        target: &str,
    ) -> Result<std::result::Result<Vec<i64>, String>> {
        let mut cleared = Vec::new();
        for r in removals.iter().filter(|r| {
            r.kind == RemovalKind::Keep
                && r.keep.is_some()
                && super::plans::clears(&r.target, target)
        }) {
            let row: Option<(i64, String)> = self.read(|c| {
                Ok(c.query_row(
                    "SELECT seq, state FROM build_items WHERE build_id = ?1 AND kind = 'remove' AND target = ?2",
                    params![build, r.target],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?)
            })?;
            let Some((seq, state)) = row else { continue };
            cleared.push(seq);
            // A file kept aside already, by the build or by its recovery, may have left its folder in place.
            if state == "applied" {
                remove_layout_dir(self, dir, folder, &r.target)?;
                continue;
            }
            let k = Keep {
                folder,
                build,
                seq,
                generation,
                keep: r.keep.as_ref().expect("filtered"),
                hashed: None,
                original: None,
            };
            if let Err(why) = self.keep_aside((dir, library), retention, &k)? {
                return Ok(Err(why));
            }
            self.fault_point(Fault::Cleared(seq));
            remove_layout_dir(self, dir, folder, &r.target)?;
        }
        Ok(Ok(cleared))
    }

    /// Finishes putting in place `p`, a disc of a committed unit of Build `build` in Library Folder `folder` at
    /// `generation`: clears its release's other disc layout out of the way, keeps aside what the plan found at its
    /// target, if that is still there, and renames its staged file into place. `Err` says why it can't be; the
    /// folder is left as it is.
    fn finish_disc(
        &self,
        (dir, library): (&FolderDir, &Tally),
        temps: &HashMap<String, Identity>,
        (folder, build, generation): (i64, i64, u64),
        p: &Publishing,
        item: &PlanItem,
        removals: &[PlanRemoval],
    ) -> Result<std::result::Result<(), String>> {
        // The release's files in its other disc layout are cleared out of the way first: until they are, the target
        // may be a folder, or in a folder that is a file.
        if removals.iter().any(|r| {
            r.kind == RemovalKind::Keep
                && r.keep.is_some()
                && super::plans::clears(&r.target, &p.target)
        }) {
            let retention = match open_retention(self, folder, dir, generation, true)? {
                Ok(Some(retention)) => retention,
                Ok(None) => return Ok(Err("the retention area was not created".into())),
                Err(why) => return Ok(Err(why)),
            };
            let cleared = self.clear_way(
                (dir, library),
                &retention,
                (folder, build, generation),
                removals,
                &p.target,
            )?;
            if let Err(why) = cleared {
                return Ok(Err(why));
            }
        }
        let ours = match dir.open_file(&p.target) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Ok(Err(e.to_string())),
            // The staged file itself, renamed into place: the same file, not only the same bytes.
            Ok(file) => match (Identity::of_fd(&file), sha256_of(&file, library)) {
                (Ok(id), Ok((sha256, size))) => Some(
                    (p.dev, p.ino) == (Some(id.to_sql().0), Some(id.to_sql().1))
                        && Some(&sha256) == p.sha256.as_ref()
                        && Some(size as i64) == p.size,
                ),
                (Err(e), _) | (_, Err(e)) => return Ok(Err(e.to_string())),
            },
        };
        match (ours, &item.keep) {
            (Some(true), _) => return Ok(Ok(())),
            (Some(false), None) => {
                return Ok(Err(format!(
                    "something else is at {} now; it was left as it is",
                    p.target
                )));
            }
            (Some(false), Some(keep)) => {
                let retention = match open_retention(self, folder, dir, generation, true)? {
                    Ok(Some(retention)) => retention,
                    Ok(None) => return Ok(Err("the retention area was not created".into())),
                    Err(why) => return Ok(Err(why)),
                };
                let k = Keep {
                    folder,
                    build,
                    seq: p.seq,
                    generation,
                    keep,
                    hashed: None,
                    original: None,
                };
                if let Err(why) = self.keep_aside((dir, library), &retention, &k)? {
                    return Ok(Err(why));
                }
            }
            (None, _) => {}
        }
        match finish_unit((dir, library), temps, p) {
            Ok(made) => {
                record_layout_dir(self, folder, made)?;
                Ok(Ok(()))
            }
            Err(why) => Ok(Err(why)),
        }
    }

    fn clean_folder(
        &self,
        (build, library): (i64, &Tally),
        unfinished: &Unfinished,
        folder: &super::folders::FolderRow,
        store: String,
    ) -> Result<std::result::Result<(), String>> {
        let roots = &*self.roots();
        let dir = match open_writable_folder(roots, &folder.root, &folder.path) {
            Ok(dir) => dir,
            Err(reason) => return Ok(Err(reason)),
        };
        let marker = match read_marker(&dir) {
            MarkerState::Present(m) if m.store == store && m.folder == folder.id => m,
            MarkerState::Present(_) => {
                return Ok(Err(
                    "its marker is for another folder or another store; the folder was left as it is".into(),
                ));
            }
            MarkerState::Absent => {
                return Ok(Err("it has no marker; the folder was left as it is".into()));
            }
            MarkerState::Unreadable(e) => {
                return Ok(Err(format!(
                    "its marker cannot be read ({e}); the folder was left as it is"
                )));
            }
        };
        let forward = if marker.generation == folder.generation {
            false
        } else if marker.generation + 1 == folder.generation
            && unfinished.finalizing
            && unfinished.generation == folder.generation
            && folder.current_build == Some(build)
        {
            true
        } else if marker.generation > folder.generation {
            return Ok(Err(format!(
                "its marker is at build generation {}, ahead of the store's {}, as when the store is restored from a \
                 backup taken before later builds; the folder was left as it is",
                marker.generation, folder.generation
            )));
        } else {
            return Ok(Err(format!(
                "its marker is at build generation {}, behind the store's {} with no record of build {build} \
                 advancing it; the folder was left as it is",
                marker.generation, folder.generation
            )));
        };
        if let Err(reason) = settle_dir_temps(self, folder.id, &dir)? {
            return Ok(Err(reason));
        }
        // Read once the directories being created are settled: a staging directory renamed into place is then
        // recorded among them.
        let temps = self.read(|c| temps(c, folder.id))?;
        if let Err(reason) = self.settle_keeping(&dir, folder.id, build)? {
            return Ok(Err(reason));
        }

        match dir.stat(MARKER_NEW) {
            Ok(None) => {}
            Ok(Some(_)) => match temps.get(MARKER_NEW) {
                Some(&id) => {
                    if let Err(reason) = remove_staged(&dir, MARKER_NEW, id) {
                        return Ok(Err(reason));
                    }
                }
                None => return Ok(Err(not_ours(&dir, MARKER_NEW))),
            },
            Err(e) => return Ok(Err(e.to_string())),
        }

        if !unfinished.publishing.is_empty() {
            let content = self.plan(unfinished.plan)?.content;
            let items = match &content.cleanup {
                Some(c) => cleanup::written_items(c),
                None => content.items,
            };
            // Each unit committed to go in place is finished first: a disc the Build stopped before keeping aside
            // what was at its target, or before renaming, is put in place from its staged file now. One that can't
            // be holds the folder, every staged file and kept file kept, until a later recovery can.
            let committing: Vec<String> = self.read(|c| {
                let mut stmt = c.prepare(
                    "SELECT unit FROM build_units WHERE build_id = ?1 AND state = 'committing' ORDER BY unit",
                )?;
                let rows = stmt.query_map([build], |r| r.get(0))?;
                Ok(rows.collect::<Result<_, _>>()?)
            })?;
            for release in &committing {
                for p in unfinished
                    .publishing
                    .iter()
                    .filter(|p| p.unit.as_ref() == Some(release))
                {
                    let item = items
                        .iter()
                        .find(|i| i.kind.writes() && i.target == p.target)
                        .ok_or_else(|| {
                            Error::Io(format!("build {build}: no plan item for {}", p.target))
                        })?;
                    if let Err(reason) = check_marker(&dir, &marker) {
                        return Ok(Err(reason));
                    }
                    let done = self.finish_disc(
                        (&dir, library),
                        &temps,
                        (folder.id, build, unfinished.generation),
                        p,
                        item,
                        &content.removals,
                    )?;
                    if let Err(why) = done {
                        return Ok(Err(unit_stuck(release, &p.target, &why)));
                    }
                }
            }
            for p in &unfinished.publishing {
                if let (Some(_), Some(c)) = (&p.origin, &content.cleanup) {
                    if let Err(reason) = check_marker(&dir, &marker) {
                        return Ok(Err(reason));
                    }
                    if let Err(reason) = self.settle_move(&dir, build, folder.id, p, c)? {
                        return Ok(Err(reason));
                    }
                    continue;
                }
                let item = items
                    .iter()
                    .find(|i| i.kind.writes() && i.target == p.target)
                    .ok_or_else(|| {
                        Error::Io(format!("build {build}: no plan item for {}", p.target))
                    })?;
                if let Err(reason) = check_marker(&dir, &marker) {
                    return Ok(Err(reason));
                }
                let found = match dir.open_file(&p.target) {
                    Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                    Err(e) => Some(Err(e.to_string())),
                    Ok(file) => Some(
                        sha256_of(&file, library)
                            .and_then(|hash| Ok((hash, rustix::fs::fstat(&file)?)))
                            .map_err(|e| e.to_string()),
                    ),
                };
                // The file written is recorded in place only once its rename is durable: the Build may have stopped
                // before it synced either folder, and a rename that is not durable may yet be undone.
                if let Some(Ok(((sha256, size), _))) = &found
                    && Some(sha256) == p.sha256.as_ref()
                    && Some(*size as i64) == p.size
                {
                    let staging = match dir.stat(STAGING) {
                        Ok(Some(st))
                            if FolderDir::kind(&st) == rustix::fs::FileType::Directory
                                && temps.get(STAGING) == Some(&Identity::of(&st)) =>
                        {
                            Some(dir.sub(STAGING))
                        }
                        Ok(_) => None,
                        Err(e) => return Ok(Err(e.to_string())),
                    };
                    // The folder it was renamed into is synced too: the system folder, or the disc layout folder it
                    // is in.
                    let synced = staging
                        .transpose()
                        .and_then(|staging| staging.map_or(Ok(()), |s| s.sync()))
                        .and_then(|()| dir.sync())
                        .and_then(|()| sync_parent(&dir, &p.target));
                    if let Err(e) = synced {
                        return Ok(Err(e.to_string()));
                    }
                    self.fault_point(Fault::ParentSynced(p.seq));
                    self.fault_point(Fault::AfterPublish(p.seq));
                }
                // A disc of a committed unit is never failed or left: the unit was finished above, so this is its
                // staged file, renamed into place, unless something changed it since.
                if let Some(release) = &p.unit {
                    let published = matches!(&found, Some(Ok(((sha256, size), st)))
                        if Some(sha256) == p.sha256.as_ref()
                            && Some(*size as i64) == p.size
                            && (p.dev, p.ino) == (Some(Identity::of(st).to_sql().0), Some(Identity::of(st).to_sql().1)));
                    if !published {
                        return Ok(Err(unit_stuck(
                            release,
                            &p.target,
                            "it is not the checked file the build put there",
                        )));
                    }
                }
                self.write(|tx| match found {
                    None => set_item(
                        tx,
                        build,
                        p.seq,
                        ItemState::Failed,
                        "the build stopped before the file was put in place",
                    ),
                    Some(Ok(((sha256, size), st)))
                        if Some(&sha256) == p.sha256.as_ref() && Some(size as i64) == p.size =>
                    {
                        record_applied(tx, build, folder.id, p.seq, item, &st, true).map(|_| ())
                    }
                    Some(Ok(_)) => set_item(
                        tx,
                        build,
                        p.seq,
                        ItemState::Uncertain,
                        &format!(
                            "the build stopped while putting {} in place, and the file there now is not the one it \
                             wrote; it was left as it is",
                            p.target
                        ),
                    ),
                    Some(Err(e)) => set_item(
                        tx,
                        build,
                        p.seq,
                        ItemState::Uncertain,
                        &format!(
                            "the build stopped while putting {} in place, and what is there now cannot be checked \
                             ({e}); it was left as it is",
                            p.target
                        ),
                    ),
                })?;
            }
        }

        match dir.stat(STAGING) {
            Ok(None) => {}
            Ok(Some(st)) => {
                let Some(&own) = temps.get(STAGING) else {
                    return Ok(Err(not_ours(&dir, STAGING)));
                };
                if FolderDir::kind(&st) != rustix::fs::FileType::Directory
                    || Identity::of(&st) != own
                {
                    return Ok(Err(not_ours(&dir, STAGING)));
                }
                let staging = match dir.sub(STAGING) {
                    Ok(staging) => staging,
                    Err(e) => return Ok(Err(e.to_string())),
                };
                let prefix = format!("{STAGING}/");
                let known: HashMap<String, Identity> = temps
                    .iter()
                    .filter_map(|(name, &id)| Some((name.strip_prefix(&prefix)?.to_owned(), id)))
                    .collect();
                if let Err(reason) = clear_staging(&dir, &staging, own, &known) {
                    return Ok(Err(reason));
                }
            }
            Err(e) => return Ok(Err(e.to_string())),
        }

        self.write(|tx| {
            tx.execute(
                "UPDATE build_units SET state = CASE state WHEN 'committing' THEN 'done' ELSE 'rolled_back' END
                 WHERE build_id = ?1 AND state IN ('staged', 'committing')",
                [build],
            )?;
            Ok(())
        })?;

        if forward {
            if let Err(reason) = check_marker(&dir, &marker) {
                return Ok(Err(reason));
            }
            write_marker(
                &dir,
                &Marker {
                    generation: folder.generation,
                    ..marker
                },
                &mut |tmp| record_temp(self, folder.id, MARKER_NEW, tmp),
            )?;
        }
        Ok(Ok(()))
    }

    /// Run when the service starts, after [`Store::recover_jobs`]: cleans up the folder of each Build that ended
    /// without its folder being cleaned up. A folder that is not the store's, or
    /// cannot be reached, is left as it is; returns, for each, the Build and why.
    pub fn recover_builds(&self) -> Result<Vec<(i64, String)>> {
        let builds: Vec<i64> = self.read(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM builds WHERE cleaned_at IS NULL AND state IN ('failed', 'interrupted') ORDER BY id",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })?;
        let mut blocked = Vec::new();
        for id in builds {
            // Recovery when the service starts is not a Job's I/O: no Job's meter counts it.
            if let Some(reason) = self.recover_build(id, None)? {
                blocked.push((id, reason));
            }
        }
        Ok(blocked)
    }

    pub fn build(&self, id: i64) -> Result<BuildDetail> {
        self.read(|c| {
            let tx = c.unchecked_transaction()?;
            Ok(BuildDetail {
                build: build_by_id(&tx, id)?,
                items: build_items(&tx, id)?,
            })
        })
    }

    /// The Build of Job `job`, if it started.
    pub fn build_of_job(&self, job: i64) -> Result<Option<BuildSummary>> {
        self.read(|c| Ok(builds_where(c, "b.job_id = ?1", [job])?.into_iter().next()))
    }

    /// The files Builds wrote into Library Folder `folder`, by name.
    pub fn built_files(&self, folder: i64) -> Result<Vec<BuiltFile>> {
        self.read(|c| {
            folder_by_id(c, folder)?;
            let mut stmt = c.prepare(
                "SELECT f.target, f.game, f.kind, f.build_id, f.selection_id, f.audit_id, r.sha256, f.sources,
                        f.roms, f.size, f.mtime_ns, f.sha256, f.torrentzip
                 FROM built_files f JOIN dat_revisions r ON r.id = f.dat_revision_id
                 WHERE f.folder_id = ?1 ORDER BY f.target",
            )?;
            let rows = stmt.query_map([folder], |r| {
                let kind: String = r.get(2)?;
                Ok(BuiltFile {
                    target: r.get(0)?,
                    game: r.get(1)?,
                    kind: ItemKind::parse(&kind).unwrap_or(ItemKind::Repack),
                    build: r.get(3)?,
                    selection: r.get(4)?,
                    audit: r.get(5)?,
                    dat_revision: r.get(6)?,
                    sources: json(r, 7)?,
                    roms: json(r, 8)?,
                    file: FileStat {
                        size: r.get::<_, i64>(9)? as u64,
                        mtime_ns: r.get::<_, i64>(10)? as i128,
                    },
                    sha256: r.get(11)?,
                    torrentzip: r.get(12)?,
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }
}
