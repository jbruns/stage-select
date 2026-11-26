//! Checks of Library Folders: what is in a folder now against the store's record of what Builds put there. A check
//! only reads the folder, through the handle it was opened by and without following a symlink; what it finds is
//! written to the store alone. A built file is read only when its size or modification time differ from its record,
//! and every entry Stage Select did not build is classified as Drift, read once and then only when it changes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use rustix::fs::{FileType, Stat};
use stage_select_api::build::{
    CheckSummary, CheckedFile, DriftEntry, DriftKind, ExpectedRom, FileOutcome, FileStat,
    FolderCheck, PlacedFile,
};
use stage_select_api::storage::{Storage, StorageFormat, multidisc_dir, playlist_name, release_of};
use stage_select_api::{DatFormat, HeaderRule, Job, JobKind, JobState};
use stage_select_engine::Progress;
use stage_select_engine::build::{GameIndex, Probe, holds_as, probe_under};
use stage_select_engine::dat::Dat;
use stage_select_engine::meter::Tally;

use super::audits::choose_revision;
use super::dats::revision_by_id;
use super::folder_dir::{FolderDir, Identity, ctime_ns, file_stat};
use super::folders::{folder_by_id, is_own, marker_problem, open_folder, store_id};
use super::jobs::{finish_running, job_by_id};
use super::plans::{Built, LaidFile, generated_what, json, laid_files, read_built, to_json};
use super::sets::set_by_id;
use super::{Error, Result, Store, now};

/// A built file's record to bring up to date: only while it is still as the check read it.
pub(super) struct Update {
    target: String,
    build: i64,
    was: FileStat,
    now: FileStat,
    /// Its SHA-256, when the check read all of it; a CHD's header alone is read, so its SHA-256 is no longer known.
    sha256: Option<String>,
    torrentzip: bool,
}

/// A file as `lstat` or `fstat` saw it: what it is, and every time its content or metadata could change. What was
/// read of a file is reused only while all of it is the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Seen {
    pub file: FileStat,
    pub identity: Identity,
    pub ctime_ns: i128,
}

impl Seen {
    pub fn of(st: &Stat) -> Seen {
        Seen {
            file: file_stat(st),
            identity: Identity::of(st),
            ctime_ns: ctime_ns(st),
        }
    }
}

/// An entry Stage Select did not build that you acknowledged, as it was then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Ack {
    pub identity: Identity,
    pub file: FileStat,
}

/// The acknowledged entries of Library Folder `folder`, by name.
pub(super) fn read_acks(c: &Connection, folder: i64) -> Result<HashMap<String, Ack>> {
    let mut stmt =
        c.prepare("SELECT name, dev, ino, size, mtime_ns FROM drift_acks WHERE folder_id = ?1")?;
    let rows = stmt.query_map([folder], |r| {
        Ok((
            r.get::<_, String>(0)?,
            Ack {
                identity: Identity::from_sql(r.get(1)?, r.get(2)?),
                file: FileStat {
                    size: r.get::<_, i64>(3)? as u64,
                    mtime_ns: r.get::<_, i64>(4)? as i128,
                },
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// What a check found, before it is stored.
pub(super) struct Scan {
    pub files: Vec<CheckedFile>,
    pub drift: Vec<DriftEntry>,
    /// Entries you acknowledged, found as they were then: neither read nor reported as drift.
    pub acknowledged: Vec<DriftEntry>,
    updates: Vec<Update>,
    /// The records of Placed Games' files to bring up to date.
    placed_updates: Vec<PlacedUpdate>,
    /// What was read of each entry, by name, with the file as it was when read.
    pub probes: HashMap<String, (Seen, Probe)>,
    /// The acknowledgements the check was made with: only these can it find changed.
    acks: HashMap<String, Ack>,
    /// Every entry the check statted, as it saw it: what its permissions are compared from.
    pub seen: Vec<crate::permissions::Seen>,
}

/// A Placed Game's file a check read again and proved: as it was recorded, and as it now is and was read.
struct PlacedUpdate {
    was: PlacedFile,
    now: Stat,
    sha256: String,
}

/// The name of the file at `path`, relative to its folder with `/` separators: its last component.
fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// What a check compares the folder with.
pub(super) struct ScanInput<'a> {
    pub built: &'a BTreeMap<String, Built>,
    /// The selected Games, by their target's lowercased name.
    pub selected: &'a HashMap<String, String>,
    pub dat: Option<&'a Dat>,
    /// What the storage policy says for the folder's system.
    pub storage: Storage,
    /// The system's latest SBI pack, when the storage policy puts each disc's SBI file beside it.
    pub sbi: Option<&'a crate::sbi_packs::SbiPack>,
    /// What earlier checks read, by name.
    pub cached: &'a HashMap<String, (Seen, Probe)>,
    /// The entries you acknowledged, by name.
    pub acks: &'a HashMap<String, Ack>,
    /// Where what is read of the folder is counted: its Root's, for the Job that reads it, if any.
    pub tally: &'a Tally,
    /// The files of the Placed Games the folder holds: never drift, each checked as a built file is.
    pub placed: &'a [PlacedFile],
    /// Their Source Set's own folder, where one found missing here may be found again; none when it can't be opened.
    pub home: Option<&'a Home>,
}

/// A Placed Game's Source Set's own folder, where a check looks for its files it finds missing where placed.
pub(crate) struct Home {
    pub dir: FolderDir,
    /// For each Placed file, by its path in its Library Folder, the path in the Source Set's folder where the Source
    /// Set's latest Audit stamped a file of its recorded device and inode.
    pub by_identity: HashMap<String, String>,
}

/// What a check says of a Placed Game's file it found missing where a file of its name is in its Source Set's folder:
/// a name says nothing of what that file holds, and only an Audit, reading it, tells whether it is the Game's.
pub(crate) const FOUND_HOME: &str =
    "a file of the same name is in your source set; audit it to see whether it holds this game";

/// What a check says of a Placed Game's file it found missing where the same file, by its device and inode, is in its
/// Source Set's folder: [`FOUND_SAME`]`.0`, its path there, then [`FOUND_SAME`]`.1`. The file placed, which may have
/// changed since.
const FOUND_SAME: (&str, &str) = (
    "the same file is in your source set, as ",
    "; audit it to see whether it still holds this game",
);

/// Whether `detail`, a check's of a Placed Game's missing file, says where in its Source Set's folder it may be.
pub(crate) fn is_found_home(detail: &str) -> bool {
    detail == FOUND_HOME || (detail.starts_with(FOUND_SAME.0) && detail.ends_with(FOUND_SAME.1))
}

/// Where the Placed Game's file `placed`, missing where it was placed, is in its Source Set's folder `home`, as the
/// check's detail says it: the same file, where the latest Audit stamped one of its device and inode and it is there
/// still; or else a regular file of its name, at the path it has in its Library Folder or by its name alone at the top.
/// Only an Audit, reading it, tells whether it holds the Game.
fn found_home(home: Option<&Home>, placed: &PlacedFile) -> Option<String> {
    let home = home?;
    if let Some(at) = home.by_identity.get(&placed.path)
        && home.dir.stat(at).ok().flatten().is_some_and(|st| {
            let identity = Identity::of(&st);
            FolderDir::kind(&st) == FileType::RegularFile
                && (identity.dev, identity.ino) == (placed.dev, placed.ino)
        })
    {
        return Some(format!("{}{at}{}", FOUND_SAME.0, FOUND_SAME.1));
    }
    [placed.path.as_str(), name_of(&placed.path)]
        .iter()
        .any(|at| {
            home.dir
                .stat(at)
                .ok()
                .flatten()
                .is_some_and(|st| FolderDir::kind(&st) == FileType::RegularFile)
        })
        .then(|| FOUND_HOME.to_owned())
}

/// The names in a folder other than Stage Select's own entries, sorted, with each disc layout folder's entries in
/// place of it, and each of `disks`, as [`deepen`].
pub(super) fn listing(dir: &FolderDir, disks: &HashSet<String>) -> std::io::Result<Vec<String>> {
    let names: Vec<String> = dir
        .entries()?
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| !is_own(name))
        .collect();
    let mut names = deepen(dir, names, disks);
    names.sort();
    Ok(names)
}

/// The folders of `built` that hold the Disks of an Arcade DAT's Games, `<game>/`.
pub(super) fn disk_dirs(built: &BTreeMap<String, Built>) -> HashSet<String> {
    built
        .iter()
        .filter(|(_, b)| b.disk)
        .filter_map(|(target, _)| target.split_once('/').map(|(dir, _)| dir.to_owned()))
        .collect()
}

/// Whether `name`, at the top of a Library Folder, is a folder a disc layout puts files in: one named for the `.cue`
/// or the `.m3u` it holds.
pub(super) fn is_layout_dir(name: &str) -> bool {
    let name = name.to_lowercase();
    name.ends_with(".cue") || name.ends_with(".m3u")
}

/// `names`, a folder's top-level entries, with each disc layout folder that is a directory, and each folder of `disks`,
/// an Arcade DAT's Game's Disks', replaced by its entries, as `<folder>/<entry>`: they belong to the folder as its
/// other files do. One that is empty, or can't be read, stays itself.
pub(super) fn deepen(dir: &FolderDir, names: Vec<String>, disks: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for name in names {
        let inner = (is_layout_dir(&name) || disks.contains(&name))
            .then(|| dir.stat(&name).ok().flatten())
            .flatten()
            .filter(|st| FolderDir::kind(st) == FileType::Directory)
            .and_then(|_| dir.sub(&name).and_then(|sub| sub.entries()).ok())
            .filter(|entries| !entries.is_empty());
        match inner {
            Some(entries) => out.extend(
                entries
                    .into_iter()
                    .map(|(entry, _)| format!("{name}/{entry}")),
            ),
            None => out.push(name),
        }
    }
    out
}

/// `entries`, with each folder holding a Placed Game's file, a loose Game's, replaced by its entries, as
/// `<folder>/<entry>`, as [`deepen`] does a disc layout folder's: each file in it is a placed file, or drift. One that
/// is no directory, a symbolic link included, is empty, or can't be read, stays itself.
fn with_placed_folders(dir: &FolderDir, entries: &[String], placed: &[PlacedFile]) -> Vec<String> {
    let holding: HashSet<&str> = placed
        .iter()
        .flat_map(|p| p.path.match_indices('/').map(|(at, _)| &p.path[..at]))
        .collect();
    if holding.is_empty() {
        return entries.to_vec();
    }
    fn expand(dir: &FolderDir, holding: &HashSet<&str>, name: String, out: &mut Vec<String>) {
        let inner = holding
            .contains(name.as_str())
            .then(|| dir.stat(&name).ok().flatten())
            .flatten()
            .filter(|st| FolderDir::kind(st) == FileType::Directory)
            .and_then(|_| dir.sub(&name).and_then(|sub| sub.entries()).ok())
            .filter(|entries| !entries.is_empty());
        match inner {
            Some(inner) => {
                for (entry, _) in inner {
                    expand(dir, holding, format!("{name}/{entry}"), out);
                }
            }
            None => out.push(name),
        }
    }
    let mut out = Vec::new();
    for name in entries {
        expand(dir, &holding, name.clone(), &mut out);
    }
    out.sort();
    out
}

/// Reads the regular file `name`, found by `lstat` as `seen`, through `dir`: opened without following a symlink, and
/// only trusted when it is the same file, the same size and modification time, before and after it is read. Its
/// payloads' checksums leave out the Header `rule` finds, if any. What it reads is counted to `tally`.
fn read(
    (dir, tally): (&FolderDir, &Tally),
    name: &str,
    seen: &Stat,
    rule: Option<HeaderRule>,
) -> std::result::Result<Probe, String> {
    let changed = || "it changed while it was read".to_owned();
    let file: File = dir.open_file(name).map_err(|e| e.to_string())?;
    let before = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
    if Seen::of(&before) != Seen::of(seen) {
        return Err(changed());
    }
    let probe = probe_under(&file, rule, tally).map_err(|e| e.to_string())?;
    still(dir, name, &file, seen)?;
    Ok(probe)
}

/// The CHD header of the regular file `name`, found by `lstat` as `seen`, read through `dir` while it is the same
/// file: its header alone is read, never its compressed data.
fn chd_header(
    dir: &FolderDir,
    name: &str,
    seen: &Stat,
) -> std::result::Result<stage_select_engine::chd::Header, String> {
    let file: File = dir.open_file(name).map_err(|e| e.to_string())?;
    let before = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
    if Seen::of(&before) != Seen::of(seen) {
        return Err("it changed while it was read".to_owned());
    }
    let header = stage_select_engine::chd::header(&file)?;
    still(dir, name, &file, seen)?;
    Ok(header)
}

/// Whether `file`, opened from `name` in `dir`, and the entry at `name` are both still the file `seen`.
fn still(dir: &FolderDir, name: &str, file: &File, seen: &Stat) -> std::result::Result<(), String> {
    let after = rustix::fs::fstat(file).map_err(|e| e.to_string())?;
    match dir.stat(name) {
        Ok(Some(st))
            if FolderDir::kind(&st) == FileType::RegularFile
                && Seen::of(&after) == Seen::of(seen)
                && Seen::of(&st) == Seen::of(seen) =>
        {
            Ok(())
        }
        _ => Err("it changed while it was read".to_owned()),
    }
}

/// Reads the regular file `name` whole, with no Header left out, while it is the file a plan found as `identity` and
/// `file`: what an addition made from it records. What it reads is counted to `tally`.
pub(super) fn read_whole(
    (dir, tally): (&FolderDir, &Tally),
    name: &str,
    identity: Identity,
    file: &FileStat,
) -> std::result::Result<Probe, String> {
    match dir.stat(name) {
        Ok(Some(st)) if Identity::of(&st) == identity && file_stat(&st) == *file => {
            read((dir, tally), name, &st, None)
        }
        Ok(_) => Err("it changed since the folder was checked".to_owned()),
        Err(e) => Err(e.to_string()),
    }
}

/// Reuses `cached`, what an earlier check read of `name`, only when the file found by `lstat` as `seen` is the one
/// it was read from: the same identity, size, modification time and status change time, through the folder's handle
/// as well as by name, and it was read under the Header Rule `rule`. Otherwise reads it again, under `rule`, what it
/// reads counted to `tally`.
pub(super) fn reuse(
    (dir, tally): (&FolderDir, &Tally),
    name: &str,
    seen: &Stat,
    cached: Option<&(Seen, Probe)>,
    rule: Option<HeaderRule>,
) -> (std::result::Result<Probe, String>, bool) {
    if let Some((was, probe)) = cached
        && *was == Seen::of(seen)
        && probe.header_rule == rule
        && let Ok(file) = dir.open_file(name)
        && still(dir, name, &file, seen).is_ok()
    {
        return (Ok(probe.clone()), false);
    }
    (read((dir, tally), name, seen, rule), true)
}

/// The files the storage policy stores games of `dat` as other than `<game>.zip`, by game: loose, as CHD, or laid out
/// as a disc layout says; none unless it says loose, loose folder or CHD.
pub(crate) fn loose_names(storage: Storage, dat: Option<&Dat>) -> HashMap<String, Vec<String>> {
    // An Arcade DAT's Game is `<game>.zip` whatever the storage policy says, and its Disks are `<game>/<disk>.chd`
    // (ADR 0026).
    if let Some(dat) = dat.filter(|d| d.format == DatFormat::Listxml) {
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for rom in dat.roms.iter().filter(|r| r.disk) {
            let game = &dat.games[rom.game];
            out.entry(game.clone())
                .or_insert_with(|| vec![format!("{game}.zip")])
                .push(format!("{game}/{}", rom.name));
        }
        return out;
    }
    let Some(dat) = dat.filter(|_| storage.format != StorageFormat::TorrentZip) else {
        return HashMap::new();
    };
    super::plans::laid_out(storage, dat)
        .into_iter()
        .filter(|(_, laid)| laid.format != StorageFormat::TorrentZip)
        .map(|(game, laid)| (game, laid.files))
        .collect()
}

/// The selected games of `selected`, keyed by `<game>.zip` lowercased, by the lowercased name of each file the
/// storage policy stores each as, given the games it stores otherwise than TorrentZip; and a Multi-disc Game's
/// release by its `.m3u`.
pub(crate) fn storage_targets(
    selected: &HashMap<String, String>,
    loose: &HashMap<String, Vec<String>>,
) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for (key, game) in selected {
        match loose.get(game) {
            Some(targets) => {
                for target in targets {
                    out.insert(target.to_lowercase(), game.clone());
                }
                let release = release_of(game);
                if targets
                    .first()
                    .is_some_and(|t| t.starts_with(&format!("{}/", multidisc_dir(&release))))
                {
                    out.insert(playlist_name(&release).to_lowercase(), release);
                }
            }
            None => {
                out.insert(key.clone(), game.clone());
            }
        }
    }
    out
}

/// Checks the folder `dir`, whose entries are `entries`, against `input`. Nothing in it is changed.
pub(super) fn scan(
    dir: &FolderDir,
    entries: &[String],
    input: &ScanInput,
    progress: Option<&(dyn Fn(Progress) + Sync)>,
) -> Scan {
    let entries = &with_placed_folders(dir, entries, input.placed);
    let loose = loose_names(input.storage, input.dat);
    let selected = &storage_targets(input.selected, &loose);
    let laid = input
        .dat
        .filter(|_| input.storage.format != StorageFormat::TorrentZip)
        .map(|dat| {
            laid_files(
                input.storage,
                dat,
                input.sbi,
                input.selected.values().map(String::as_str),
            )
        })
        .unwrap_or_default();
    let mut out = Scan {
        files: Vec::new(),
        drift: Vec::new(),
        acknowledged: Vec::new(),
        updates: Vec::new(),
        placed_updates: Vec::new(),
        probes: HashMap::new(),
        acks: input.acks.clone(),
        seen: Vec::new(),
    };
    let by_lower: HashMap<String, &str> = entries
        .iter()
        .map(|name| (name.to_lowercase(), name.as_str()))
        .collect();
    let total = input.built.len() + input.placed.len() + entries.len();
    let mut done = 0;
    let mut tick = || {
        done += 1;
        if let Some(progress) = progress {
            progress(Progress {
                files_done: done,
                files_total: total,
            });
        }
    };
    // Reads `name` under the Header Rule `rule`, or reuses what an earlier check read of it under that rule while it
    // is unchanged since.
    let probe_of = |name: &str, st: &Stat, rule: Option<HeaderRule>, out: &mut Scan| {
        let (probe, hashed) = reuse((dir, input.tally), name, st, input.cached.get(name), rule);
        if let Ok(p) = &probe {
            out.probes
                .insert(name.to_owned(), (Seen::of(st), p.clone()));
        }
        (probe, hashed)
    };

    let mut consumed = HashSet::new();
    for (target, built) in input.built {
        let mut file = CheckedFile {
            target: target.clone(),
            game: built.game.clone(),
            outcome: FileOutcome::Unchanged,
            hashed: false,
            torrentzip: built.torrentzip || built.loose || built.chd,
            selected: selected.contains_key(&target.to_lowercase()),
            placed: input.placed.iter().any(|p| p.path == *target),
            detail: None,
        };
        let found = by_lower.get(&target.to_lowercase()).copied();
        if let Some(name) = found {
            consumed.insert(name.to_owned());
        }
        let st = match found {
            None => None,
            Some(name) if name != target => {
                file.outcome = FileOutcome::Conflict;
                file.detail = Some(format!("it is now named {name}"));
                None
            }
            Some(_) => match dir.stat(target) {
                Ok(st) => {
                    out.seen.extend(
                        st.as_ref()
                            .and_then(|st| crate::permissions::Seen::of(target, st)),
                    );
                    st
                }
                Err(e) => {
                    file.outcome = FileOutcome::Unreadable;
                    file.detail = Some(e.to_string());
                    None
                }
            },
        };
        match st {
            None if found.is_none() => {
                file.outcome = FileOutcome::Missing;
                // A file both built and placed is its Game's only copy: say where it may be.
                file.detail = input
                    .placed
                    .iter()
                    .find(|p| p.path == *target)
                    .and_then(|p| found_home(input.home, p));
            }
            None => {}
            Some(st) if FolderDir::kind(&st) != FileType::RegularFile => {
                file.outcome = FileOutcome::Conflict;
                file.detail = Some("it is no longer a regular file".into());
            }
            Some(st) if file_stat(&st) == built.file => {}
            // A CHD's data was checked against the DAT when it was built: it is the same CHD while the SHA-1 its
            // header holds, of its data and metadata, is the one recorded then, however it was recompressed since.
            Some(st) if built.chd => match chd_header(dir, target, &st) {
                Err(e) => {
                    file.outcome = FileOutcome::Unreadable;
                    file.detail = Some(e);
                }
                Ok(header) if built.chd_sha1.as_deref() == Some(header.sha1.as_str()) => {
                    file.outcome = FileOutcome::Refreshed;
                    out.updates.push(Update {
                        target: target.clone(),
                        build: built.build,
                        was: built.file.clone(),
                        now: file_stat(&st),
                        sha256: None,
                        torrentzip: false,
                    });
                }
                Ok(header) => {
                    file.outcome = FileOutcome::Conflict;
                    let what = if built.chd_raw_sha1.as_deref() == Some(header.raw_sha1.as_str()) {
                        "its CHD holds the same data, but its metadata, such as its track list, is not what was \
                         checked against the DAT"
                    } else {
                        "its CHD no longer holds the data that was checked against the DAT"
                    };
                    file.detail = Some(format!("it changed since it was built, and {what}"));
                }
            },
            Some(st) => {
                // A built file is known by what it was built from: an addition is hashed whole, whatever the DAT's rule.
                let rule = built.roms.iter().find_map(|r| r.header);
                let (probe, hashed) = probe_of(target, &st, rule, &mut out);
                file.hashed = hashed;
                let now = file_stat(&st);
                match probe {
                    Err(e) => {
                        file.outcome = FileOutcome::Unreadable;
                        file.detail = Some(e);
                    }
                    Ok(p) if built.sha256.as_deref() == Some(p.sha256.as_str()) => {
                        file.outcome = FileOutcome::Refreshed;
                        out.updates.push(Update {
                            target: target.clone(),
                            build: built.build,
                            was: built.file.clone(),
                            now,
                            sha256: Some(p.sha256),
                            torrentzip: built.torrentzip,
                        });
                    }
                    // A generated file, an `.m3u` or an SBI file, holds no ROM: only its SHA-256 says it is the same.
                    Ok(p) if !built.roms.is_empty() && holds_as(&p, &built.roms, built.loose) => {
                        file.outcome = FileOutcome::Repacked;
                        file.torrentzip = p.torrentzip || built.loose;
                        if !file.torrentzip {
                            file.detail = Some("not TorrentZip".into());
                        }
                        out.updates.push(Update {
                            target: target.clone(),
                            build: built.build,
                            was: built.file.clone(),
                            now,
                            sha256: Some(p.sha256),
                            torrentzip: p.torrentzip,
                        });
                    }
                    Ok(_) if built.roms.is_empty() => {
                        file.outcome = FileOutcome::Conflict;
                        file.detail = Some(
                            "it changed since it was built, and no longer holds what was written there".into(),
                        );
                    }
                    Ok(_) => {
                        file.outcome = FileOutcome::Conflict;
                        file.detail = Some(
                            "it changed since it was built and no longer holds the ROMs the DAT expects there"
                                .into(),
                        );
                    }
                }
            }
        }
        // A file both built and placed is its Game's only copy too: trusted unread only while it is the same file,
        // unchanged, by its device, inode and change time as well.
        if let (Some(placed), Some(st)) = (input.placed.iter().find(|p| p.path == *target), st)
            && FolderDir::kind(&st) == FileType::RegularFile
            && !crate::placements::unchanged(placed, &st)
            && !matches!(file.outcome, FileOutcome::Missing | FileOutcome::Unreadable)
        {
            let was = PlacedAs {
                roms: &built.roms,
                loose: built.loose,
            };
            let (probe, hashed) = probe_of(target, &st, was.header(), &mut out);
            file.hashed |= hashed;
            match probe.map(|p| (proven(placed, &p, Some(was)), p)) {
                Err(e) => {
                    file.outcome = FileOutcome::Unreadable;
                    file.detail = Some(e);
                }
                Ok((Ok(()), p)) => {
                    if file.outcome == FileOutcome::Unchanged {
                        file.outcome = FileOutcome::Refreshed;
                    }
                    out.placed_updates.push(PlacedUpdate {
                        was: placed.clone(),
                        now: st,
                        sha256: p.sha256,
                    });
                }
                Ok((Err(why), _)) => {
                    file.outcome = FileOutcome::Conflict;
                    file.detail = Some(why);
                }
            }
        }
        out.files.push(file);
        tick();
    }

    // A Placed Game's file is its Source Set's only copy: changed or missing, it is reported as a built file is, and
    // never as drift.
    for placed in input.placed {
        let target = &placed.path;
        if input.built.contains_key(target) {
            tick();
            continue;
        }
        let mut file = CheckedFile {
            target: target.clone(),
            game: placed.game.clone(),
            outcome: FileOutcome::Unchanged,
            hashed: false,
            torrentzip: true,
            selected: selected.contains_key(&target.to_lowercase()),
            placed: true,
            detail: None,
        };
        let found = by_lower.get(&target.to_lowercase()).copied();
        if let Some(name) = found {
            consumed.insert(name.to_owned());
        }
        let found = found.map(|name| (name, dir.stat(name)));
        if let Some((name, Ok(Some(st)))) = &found {
            out.seen.extend(crate::permissions::Seen::of(*name, st));
        }
        match found {
            None => {
                file.outcome = FileOutcome::Missing;
                file.detail = found_home(input.home, placed);
            }
            Some((name, _)) if name != target => {
                file.outcome = FileOutcome::Conflict;
                file.detail = Some(format!("it is now named {name}"));
            }
            Some((_, Err(e))) => {
                file.outcome = FileOutcome::Unreadable;
                file.detail = Some(e.to_string());
            }
            Some((_, Ok(None))) => file.outcome = FileOutcome::Missing,
            Some((_, Ok(Some(st)))) if FolderDir::kind(&st) != FileType::RegularFile => {
                file.outcome = FileOutcome::Conflict;
                file.detail = Some("it is no longer a regular file".into());
            }
            // Trusted unread only while it is the same file, unchanged: one put in its place, or written over with
            // its size and modification time set back, is read again.
            Some((_, Ok(Some(st)))) if crate::placements::unchanged(placed, &st) => {}
            Some((_, Ok(Some(st)))) => {
                // Nothing records the ROMs it was placed as: only its SHA-256 can prove it.
                let (probe, hashed) = probe_of(target, &st, None, &mut out);
                file.hashed = hashed;
                match probe.map(|p| (proven(placed, &p, None), p)) {
                    Err(e) => {
                        file.outcome = FileOutcome::Unreadable;
                        file.detail = Some(e);
                    }
                    Ok((Ok(()), p)) => {
                        file.outcome = FileOutcome::Refreshed;
                        out.placed_updates.push(PlacedUpdate {
                            was: placed.clone(),
                            now: st,
                            sha256: p.sha256,
                        });
                    }
                    Ok((Err(why), _)) => {
                        file.outcome = FileOutcome::Conflict;
                        file.detail = Some(why);
                    }
                }
            }
        }
        out.files.push(file);
        tick();
    }

    let index = input.dat.map(GameIndex::new);
    for name in entries {
        if consumed.contains(name) {
            tick();
            continue;
        }
        let lower = name.to_lowercase();
        let mut entry = DriftEntry {
            name: name.clone(),
            kind: DriftKind::Unknown,
            game: None,
            conflict: selected.contains_key(&lower),
            hashed: false,
            detail: None,
        };
        let stat = dir.stat(name);
        if let Ok(Some(st)) = &stat {
            out.seen
                .extend(crate::permissions::Seen::of(name.as_str(), st));
        }
        match stat {
            Ok(None) => {
                tick();
                continue;
            }
            // Acknowledged and unchanged since, by identity, size and modification time: not read again.
            Ok(Some(st))
                if input.acks.get(name).is_some_and(|ack| {
                    ack.identity == Identity::of(&st) && ack.file == file_stat(&st)
                }) =>
            {
                if FolderDir::kind(&st) == FileType::Directory {
                    entry.kind = DriftKind::Directory;
                }
                out.acknowledged.push(entry);
                tick();
                continue;
            }
            Err(e) => entry.detail = Some(e.to_string()),
            Ok(Some(st)) => match FolderDir::kind(&st) {
                FileType::Directory => entry.kind = DriftKind::Directory,
                FileType::RegularFile => {
                    let (probe, hashed) =
                        probe_of(name, &st, input.dat.and_then(Dat::header), &mut out);
                    entry.hashed = hashed;
                    match probe {
                        Err(e) => entry.detail = Some(format!("it cannot be read: {e}")),
                        Ok(p) => match &index {
                            None => {
                                entry.detail = Some(
                                    "the DAT revision is no longer kept, so it was not compared"
                                        .into(),
                                );
                            }
                            Some(index) => {
                                if let Some(game) =
                                    index.game_of(&p).filter(|_| !laid.contains_key(name))
                                {
                                    entry.game = Some(game.to_owned());
                                    let named = loose
                                        .get(game)
                                        .cloned()
                                        .unwrap_or_else(|| vec![format!("{game}.zip")]);
                                    entry.kind = if !named.contains(name) {
                                        DriftKind::Renamed
                                    } else if selected.contains_key(&lower) {
                                        DriftKind::Selected
                                    } else {
                                        DriftKind::Unselected
                                    };
                                } else if let Some(file) = laid.get(name) {
                                    // A disc layout's file is told by where it is: a cleanup can adopt it there.
                                    let adoptable = match file {
                                        LaidFile::Chd { .. } => chd_header(dir, name, &st)
                                            .map(|_| {
                                                Some(
                                                    "it is checked against the DAT when you adopt it, by \
                                                     extracting it once"
                                                        .to_owned(),
                                                )
                                            })
                                            .map_err(|e| format!("it is not a CHD that can be read: {e}")),
                                        _ if file.holds(&p) => Ok(None),
                                        LaidFile::Part { rom, .. } => Err(format!(
                                            "it doesn't hold {}, the ROM the DAT expects there",
                                            rom.name
                                        )),
                                        LaidFile::Generated { file, .. } => Err(format!(
                                            "it isn't exactly the {} a build writes",
                                            generated_what(file.kind)
                                        )),
                                    };
                                    match adoptable {
                                        Ok(detail) => {
                                            entry.game = Some(file.game().to_owned());
                                            entry.kind = DriftKind::Selected;
                                            entry.detail = detail;
                                        }
                                        Err(why) => entry.detail = Some(why),
                                    }
                                }
                            }
                        },
                    }
                }
                _ => {
                    entry.detail = Some("it is not a regular file, so it was not opened".into());
                }
            },
        }
        out.drift.push(entry);
        tick();
    }
    out
}

/// What a Placed Game's file was placed as: the ROMs the Build that placed it recorded it holding, from that Build's
/// DAT revision, and whether it is one of them, loose.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PlacedAs<'a> {
    pub roms: &'a [ExpectedRom],
    pub loose: bool,
}

impl PlacedAs<'_> {
    /// The Header Rule its ROMs' checksums leave a Header out under, if any.
    pub fn header(&self) -> Option<HeaderRule> {
        self.roms.iter().find_map(|r| r.header)
    }
}

/// Whether the Placed Game's file `placed`, its stamp moved since it was recorded, still holds what it was placed as,
/// read again as `probe` under its ROMs' Header Rule: its SHA-256 as recorded, or, with none recorded, as for a file
/// placed by a rename before it kept its Audit's, its payloads exactly the ROMs it was placed as, `placed_as`, each
/// listed with a SHA-1 or SHA-256 that its payload has: a structurally valid TorrentZip holding exactly them, whatever
/// its deflate streams, as another TorrentZip tool may have written it; or a loose file that is the one of them,
/// whole, with no Header left out. A ZIP member's Header under a Header Rule is left out of the proof, as an Audit
/// leaves it out of its Finding: the Operator's choice, so a Header edited by hand isn't detected. A loose file with
/// one isn't one ROM whole, and is placed converted, with its SHA-256. Otherwise why not.
pub(crate) fn proven(
    placed: &PlacedFile,
    probe: &Probe,
    placed_as: Option<PlacedAs>,
) -> std::result::Result<(), String> {
    let changed =
        "it changed since it was placed; an audit of its source set says what it holds now";
    if let Some(sha256) = &placed.sha256 {
        return match *sha256 == probe.sha256 {
            true => Ok(()),
            false => Err(changed.into()),
        };
    }
    // A CHD is placed read, so always with its SHA-256.
    if placed.chd {
        return Err(changed.into());
    }
    let Some(was) = placed_as else {
        return Err(format!(
            "{changed}; nothing records the ROMs it was placed as, to check it against"
        ));
    };
    if was.roms.is_empty() || !was.roms.iter().all(identified) {
        return Err(format!(
            "{changed}; the DAT doesn't list a SHA-1 or SHA-256 for every one of its ROMs, to check it against"
        ));
    }
    let exact = match was.loose {
        false => {
            probe.zip && probe.torrentzip && stage_select_engine::build::holds(probe, was.roms)
        }
        // A loose file is one ROM of its Game, named for it.
        true => {
            let name = name_of(&placed.path);
            probe
                .payloads
                .iter()
                .all(|p| p.checksums().header.is_none())
                && was.roms.iter().all(|rom| name_of(&rom.name) == name)
                && holds_as(probe, was.roms, true)
        }
    };
    match exact {
        true => Ok(()),
        false => Err(format!(
            "it changed since it was placed, and isn't exactly the ROMs {} was placed with; an audit of its \
             source set says what it holds now",
            placed.game
        )),
    }
}

/// Whether the DAT lists `rom` with a SHA-1 or SHA-256: a checksum that says what its bytes are, as a size, CRC-32 or
/// MD5 doesn't.
pub(crate) fn identified(rom: &ExpectedRom) -> bool {
    rom.sha1.is_some() || rom.sha256.is_some()
}

impl Scan {
    pub fn summary(&self) -> CheckSummary {
        let mut s = CheckSummary::default();
        for f in &self.files {
            *match f.outcome {
                FileOutcome::Unchanged => &mut s.unchanged,
                FileOutcome::Refreshed => &mut s.refreshed,
                FileOutcome::Repacked => &mut s.repacked,
                FileOutcome::Conflict => &mut s.conflict,
                FileOutcome::Missing => &mut s.missing,
                FileOutcome::Unreadable => &mut s.unreadable,
            } += 1;
            s.hashed += f.hashed as u64;
        }
        for d in &self.drift {
            s.conflict += d.conflict as u64;
            s.hashed += d.hashed as u64;
        }
        s.drift = self.drift.len() as u64;
        s.acknowledged = self.acknowledged.len() as u64;
        s
    }
}

/// The selected Games of Selection `selection`, by their target's lowercased name.
pub(super) fn selected_targets(c: &Connection, selection: i64) -> Result<HashMap<String, String>> {
    let mut stmt = c.prepare(
        "SELECT title FROM selection_releases WHERE selection_id = ?1 AND outcome = 'kept'",
    )?;
    let rows = stmt.query_map([selection], |r| r.get::<_, String>(0))?;
    let mut selected = HashMap::new();
    for title in rows {
        let title = title?;
        selected.insert(format!("{title}.zip").to_lowercase(), title);
    }
    Ok(selected)
}

pub(super) fn read_probes(c: &Connection, folder: i64) -> Result<HashMap<String, (Seen, Probe)>> {
    let mut stmt = c.prepare(
        "SELECT name, size, mtime_ns, dev, ino, ctime_ns, probe FROM folder_probes WHERE folder_id = ?1",
    )?;
    let rows = stmt.query_map([folder], |r| {
        Ok((
            r.get::<_, String>(0)?,
            (
                Seen {
                    file: FileStat {
                        size: r.get::<_, i64>(1)? as u64,
                        mtime_ns: r.get::<_, i64>(2)? as i128,
                    },
                    identity: Identity::from_sql(r.get(3)?, r.get(4)?),
                    ctime_ns: r.get::<_, i64>(5)? as i128,
                },
                json(r, 6)?,
            ),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Stores a check of Library Folder `folder`: brings up to date each built file's record that is still as the check
/// read it, keeps what it read for the next check, and records the report. Returns the stored check.
pub(super) fn store_check(
    tx: &Transaction,
    folder: i64,
    job: Option<i64>,
    scan: Option<&Scan>,
    mut check: FolderCheck,
) -> Result<FolderCheck> {
    if let Some(scan) = scan {
        for u in &scan.updates {
            tx.execute(
                "UPDATE built_files SET size = ?6, mtime_ns = ?7, sha256 = ?8, torrentzip = ?9
                 WHERE folder_id = ?1 AND target = ?2 AND build_id = ?3 AND size = ?4 AND mtime_ns = ?5",
                params![
                    folder,
                    u.target,
                    u.build,
                    u.was.size as i64,
                    u.was.mtime_ns as i64,
                    u.now.size as i64,
                    u.now.mtime_ns as i64,
                    u.sha256,
                    u.torrentzip
                ],
            )?;
        }
        for u in &scan.placed_updates {
            let now = crate::placements::stamp_of(&u.now);
            crate::placements::restamp(tx, folder, &u.was.path, &u.was, (&now, &u.sha256))?;
        }
        tx.execute("DELETE FROM folder_probes WHERE folder_id = ?1", [folder])?;
        // An acknowledgement lasts only while its entry is as it was: one the check found changed, or gone, is
        // dropped, and the entry is drift again. Only one the check was made with, and still as it was then, is: one
        // made, or made again, since the check read the store is not what it found changed.
        let kept: HashSet<&str> = scan.acknowledged.iter().map(|d| d.name.as_str()).collect();
        for (name, ack) in scan.acks.iter().filter(|(n, _)| !kept.contains(n.as_str())) {
            let (dev, ino) = ack.identity.to_sql();
            tx.execute(
                "DELETE FROM drift_acks
                 WHERE folder_id = ?1 AND name = ?2 AND dev = ?3 AND ino = ?4 AND size = ?5 AND mtime_ns = ?6",
                params![
                    folder,
                    name,
                    dev,
                    ino,
                    ack.file.size as i64,
                    ack.file.mtime_ns as i64
                ],
            )?;
        }
        for (name, (at, probe)) in &scan.probes {
            let (dev, ino) = at.identity.to_sql();
            tx.execute(
                "INSERT INTO folder_probes (folder_id, name, size, mtime_ns, dev, ino, ctime_ns, probe)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    folder,
                    name,
                    at.file.size as i64,
                    at.file.mtime_ns as i64,
                    dev,
                    ino,
                    at.ctime_ns as i64,
                    to_json(probe)
                ],
            )?;
        }
    }
    let before = latest_summary(tx, folder)?.map_or(0, |s| s.drift);
    if check.summary.drift > before {
        let (root, path) = tx.query_row(
            "SELECT root, path FROM library_folders WHERE id = ?1",
            [folder],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        crate::observe::note(crate::observe::Observed::Drift {
            folder,
            root,
            path,
            drift: check.summary.drift,
        });
    }
    check.summary.job = job;
    check.summary.at = now();
    tx.execute(
        "INSERT INTO folder_checks (folder_id, job_id, generation, created_at, summary, report)
         VALUES (?1, ?2, ?3, ?4, '', '')",
        params![folder, job, check.generation as i64, check.summary.at],
    )?;
    let id = tx.last_insert_rowid();
    check.summary.id = id;
    tx.execute(
        "UPDATE folder_checks SET summary = ?2, report = ?3 WHERE id = ?1",
        params![id, to_json(&check.summary), to_json(&check)],
    )?;
    tx.execute(
        "DELETE FROM folder_checks WHERE folder_id = ?1 AND id < ?2
         AND id NOT IN (SELECT check_id FROM plans WHERE check_id IS NOT NULL)",
        params![folder, id],
    )?;
    Ok(check)
}

/// The latest check of Library Folder `folder`.
pub(super) fn latest_check(c: &Connection, folder: i64) -> Result<Option<FolderCheck>> {
    Ok(c.query_row(
        "SELECT report FROM folder_checks WHERE folder_id = ?1 ORDER BY id DESC LIMIT 1",
        [folder],
        |r| json(r, 0),
    )
    .optional()?)
}

/// The summary of the latest check of Library Folder `folder`.
pub(super) fn latest_summary(c: &Connection, folder: i64) -> Result<Option<CheckSummary>> {
    Ok(c.query_row(
        "SELECT summary FROM folder_checks WHERE folder_id = ?1 ORDER BY id DESC LIMIT 1",
        [folder],
        |r| json(r, 0),
    )
    .optional()?)
}

/// What a Check Job reads from the store, in one transaction.
struct CheckInputs {
    folder: super::folders::FolderRow,
    store: String,
    built: BTreeMap<String, Built>,
    selection: Option<i64>,
    selected: HashMap<String, String>,
    storage: Storage,
    revision: (String, bool),
    cached: HashMap<String, (Seen, Probe)>,
    acks: HashMap<String, Ack>,
    placed: Vec<PlacedFile>,
    /// The Root and path of the folder's Source Set.
    set: (String, String),
    /// Where the Source Set's latest Audit stamped each Placed file by its identity, as [`Home::by_identity`].
    by_identity: HashMap<String, String>,
}

impl Store {
    /// Queues a read-only Check of Library Folder `folder`, started by `actor`. Refused while the folder has a Build
    /// or a Check queued or running.
    pub fn enqueue_check(&self, folder: i64, actor: &str) -> Result<Job> {
        self.write(|tx| {
            let row = folder_by_id(tx, folder)?;
            row.check_registered()?;
            if let Some(job) = row.active_job {
                return Err(Error::Conflict(format!(
                    "library folder {folder} already has a build or check queued or running: job {job}"
                )));
            }
            let revision: i64 = match row.current_build {
                Some(build) => tx.query_row(
                    "SELECT dat_revision_id FROM builds WHERE id = ?1",
                    [build],
                    |r| r.get(0),
                )?,
                None => choose_revision(tx, &set_by_id(tx, row.source_set_id)?, None)?.0,
            };
            tx.execute(
                "INSERT INTO jobs (state, kind, source_set_id, dat_revision_id, actor, readers, hashers, created_at,
                                   library_folder_id)
                 VALUES ('queued', 'check', ?1, ?2, ?3, 1, 1, ?4, ?5)",
                params![row.source_set_id, revision, actor, now(), folder],
            )?;
            job_by_id(tx, tx.last_insert_rowid())
        })
    }

    /// Runs the Running Check Job `id`: checks its folder against the store's record, calling `progress` as entries
    /// are checked, and stores what it found. Nothing in the folder is changed. A folder that cannot be opened fails
    /// the Job. Returns the Job as it ends.
    pub fn run_check_job(
        &self,
        id: i64,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<Job> {
        // What it reads and writes is counted for it, even where it isn't handed its meter.
        let _metered = self.meter.job(id).on_this_thread();
        let job = self.job(id)?;
        if job.kind != JobKind::Check {
            return Err(Error::Refused(format!("job {id} is not a check")));
        }
        if job.state != JobState::Running {
            return Err(Error::Refused(format!(
                "job {id} is {}, not running",
                job.state.as_str()
            )));
        }
        let folder = job
            .library_folder
            .ok_or_else(|| Error::Io(format!("check job {id} has no library folder")))?;
        let outcome = self.check_folder(id, folder, progress);
        if let Err(err) = outcome {
            let message = err.to_string();
            self.write(|tx| {
                match finish_running(tx, &self.meter, id, JobState::Failed, Some(&message)) {
                    Err(Error::Io(_)) | Ok(()) => Ok(()),
                    Err(err) => Err(err),
                }
            })?;
        }
        self.job(id)
    }

    fn check_folder(
        &self,
        job: i64,
        folder: i64,
        progress: Option<&(dyn Fn(Progress) + Sync)>,
    ) -> Result<()> {
        let roots = &*self.roots();
        let inputs = self.read(|c| {
            let tx = c.unchecked_transaction()?;
            let folder = folder_by_id(&tx, folder)?;
            let selection: Option<i64> = match folder.current_build {
                Some(build) => Some(tx.query_row(
                    "SELECT selection_id FROM builds WHERE id = ?1",
                    [build],
                    |r| r.get(0),
                )?),
                None => None,
            };
            let selected = match selection {
                Some(s) => selected_targets(&tx, s)?,
                None => HashMap::new(),
            };
            let revision_id: i64 = tx.query_row(
                "SELECT dat_revision_id FROM jobs WHERE id = ?1",
                [job],
                |r| r.get(0),
            )?;
            let (_, record) = revision_by_id(&tx, revision_id)?;
            let revision = (record.sha256, record.blob_present);
            let storage = crate::storage::policy(
                &tx,
                stage_select_api::storage::StorageScope::PlayLibraries,
                &folder.system,
            )?;
            let placed = crate::placements::placed_files_in(&tx, folder.id)?;
            Ok(CheckInputs {
                storage,
                store: store_id(&tx)?,
                built: read_built(&tx, folder.id)?,
                cached: read_probes(&tx, folder.id)?,
                acks: read_acks(&tx, folder.id)?,
                by_identity: crate::placements::stamped_home(&tx, folder.source_set_id, &placed)?,
                placed,
                set: {
                    let set = set_by_id(&tx, folder.source_set_id)?;
                    (set.record.root, set.record.path)
                },
                folder,
                selection,
                selected,
                revision,
            })
        })?;
        let dir = open_folder(roots, &inputs.folder.root, &inputs.folder.path)
            .map_err(|e| Error::Refused(format!("the library folder cannot be opened: {e}")))?;
        let entries =
            listing(&dir, &disk_dirs(&inputs.built)).map_err(|e| Error::Io(e.to_string()))?;
        let home = match inputs.placed.is_empty() {
            true => None,
            false => open_folder(roots, &inputs.set.0, &inputs.set.1)
                .ok()
                .map(|dir| Home {
                    dir,
                    by_identity: inputs.by_identity.clone(),
                }),
        };
        let dat = match inputs.revision.1 {
            true => Some(Dat::read(&self.blob_path(&inputs.revision.0))?),
            false => None,
        };
        let sbi = match inputs.storage.sbi {
            true => self.latest_sbi_pack(&inputs.folder.system)?,
            false => None,
        };
        let mut check = FolderCheck {
            folder,
            summary: CheckSummary::default(),
            generation: inputs.folder.generation,
            selection: inputs.selection,
            dat_revision: Some(inputs.revision.0.clone()),
            files: Vec::new(),
            drift: Vec::new(),
            acknowledged: Vec::new(),
        };
        let problem = marker_problem(
            &dir,
            &inputs.store,
            folder,
            inputs.folder.generation,
            entries.is_empty(),
        );
        let scan = match problem {
            Some(problem) => {
                check.summary.marker = Some(problem);
                None
            }
            None => {
                let scan = scan(
                    &dir,
                    &entries,
                    &ScanInput {
                        built: &inputs.built,
                        selected: &inputs.selected,
                        dat: dat.as_ref(),
                        storage: inputs.storage,
                        sbi: sbi.as_ref(),
                        cached: &inputs.cached,
                        acks: &inputs.acks,
                        tally: &Tally::of_this_thread(Some(&inputs.folder.root)),
                        placed: &inputs.placed,
                        home: home.as_ref(),
                    },
                    progress,
                );
                check.summary = scan.summary();
                if let Some(root) = roots.root(&inputs.folder.root) {
                    let mut seen = scan.seen.clone();
                    if let Ok(st) = rustix::fs::fstat(&dir) {
                        seen.extend(crate::permissions::Seen::of("", &st));
                    }
                    self.record_permissions(
                        crate::permissions::Scope::Library(folder),
                        root,
                        &inputs.folder.path,
                        root.writable,
                        seen,
                    );
                }
                check.files = scan.files.clone();
                check.drift = scan.drift.clone();
                check.acknowledged = scan.acknowledged.clone();
                Some(scan)
            }
        };
        self.write(|tx| {
            store_check(tx, folder, Some(job), scan.as_ref(), check.clone())?;
            finish_running(tx, &self.meter, job, JobState::Succeeded, None)
        })
    }

    /// The latest check of Library Folder `folder`.
    pub fn latest_check(&self, folder: i64) -> Result<Option<FolderCheck>> {
        self.read(|c| {
            folder_by_id(c, folder)?;
            latest_check(c, folder)
        })
    }
}
