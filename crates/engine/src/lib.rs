//! Audits a Source Set against a DAT Revision (see GLOSSARY.md and ADR 0002).

pub mod build;
pub mod calibrate;
pub mod cap;
pub mod chd;
pub mod dat;
pub mod fixdat;
pub mod meter;
pub mod payload;
pub mod pool;
mod read;
pub mod sevenz;
pub mod torrentzip;
pub mod zipdir;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

pub use read::Pools;
pub use title_select_api::Workers;
pub use title_select_api::report::*;

use dat::{Dat, DumpStatus};
use payload::Checksums;
use read::{FileRead, Known, Origin, ToRead};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read DAT: {0}")]
    Dat(String),
    #[error("cannot read source set: {0}")]
    SourceSet(String),
    /// The read stopped because a worker it needed couldn't start.
    #[error("the audit stopped: {0}")]
    Workers(String),
}

/// One payload reader and one hash worker: what the stateless Audit uses.
pub const ONE_WORKER_EACH: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// How far an Audit has got: files whose Entries are read, of the files found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub files_done: usize,
    pub files_total: usize,
}

/// How an Audit runs.
pub struct Options<'a> {
    /// The DAT Revision's original filename, as the report names it.
    pub file: String,
    /// A canonical path every file and directory must resolve inside. One that resolves outside is an Unreadable
    /// Entry and is not read, and the file read is the one its resolved path names.
    pub confine: Option<&'a Path>,
    /// At least one of each: `readers` threads read files, and `hashers` threads checksum their payloads.
    pub workers: Workers,
    /// The pools `workers` share with other Jobs, which say how many of them may work at each moment: a permit is
    /// taken before each file and payload. All of them work when absent. A smaller pool holds back the next file,
    /// and stops no read under way.
    pub pools: Option<Pools<'a>>,
    /// Called as each file's Entries are read, from the reader threads.
    pub progress: Option<&'a (dyn Fn(Progress) + Sync)>,
    /// Whether ZIP archives are claimed from their directories ([`AuditEvidence::Quick`]), as the report records.
    /// Which earlier reads may be reused is up to `reads`.
    pub evidence: AuditEvidence,
    /// Earlier payload reads to reuse under the change test, and where to record this Audit's reads.
    pub reads: Option<&'a dyn PayloadReads>,
    /// Where the payload bytes read are counted: for the Job, the Source Set's Root and in total.
    pub tally: meter::Tally,
    /// The files of the Source Set's Placed Games, read as its Entries from the Library Folders that hold them.
    pub placed: &'a [Placed<'a>],
    /// How chdman verifies the Source Set's Disks (ADR 0026), in an Audit that reads payloads; none where there is no
    /// chdman to run, and each Disk's CHD such an Audit can't reuse an earlier read of is then Unreadable.
    pub disks: Option<&'a ChdAudit>,
}

/// The recorded files of a Source Set's Placed Games in one Library Folder (ADR 0023). Only these are read of it,
/// never another file there; a file that is gone is no Entry, so its Game is Missing or Incomplete.
pub struct Placed<'a> {
    /// The Library Folder's id, which names each Entry's path ([`title_select_api::storage::placed_path`]).
    pub id: i64,
    /// The canonical path of the folder's Root. Everything below it, the folder itself included, is opened without
    /// following a symbolic link.
    pub root: &'a Path,
    /// The Library Folder, relative to its Root, with `/` separators.
    pub folder: &'a str,
    /// Each file, relative to the folder, with `/` separators.
    pub files: Vec<String>,
    /// The pools of the folder's Root, as [`Options::pools`]; the Host's hash workers are the Source Set's.
    pub pools: Option<Pools<'a>>,
    /// Where the payload bytes read of it are counted: the Library Folder's Root's.
    pub tally: meter::Tally,
    /// Those of `files` that are CHDs, placed under Place Converted Games, and how to run chdman to read them.
    pub chds: Vec<PlacedChd>,
    pub chd: ChdAudit,
}

/// A Placed Game's file that is a CHD (ADR 0023): a full Audit extracts it with chdman and reads each track it gives
/// back as a payload of its own, named for the ROM it is to be; a quick inventory takes its header's SHA-1, while it is
/// the one it held when it was placed, as Claim Evidence of each. A CD's cue sheet, which a CHD doesn't hold, is read
/// from where it is kept, in the store, in either.
#[derive(Debug, Clone)]
pub struct PlacedChd {
    /// Relative to the Library Folder, with `/` separators, as in [`Placed::files`].
    pub rel: String,
    pub game: String,
    /// The SHA-1 its header held when extracting it gave back every track of its disc exactly.
    pub sha1: String,
    /// Its disc's ROMs as the DAT listed them when it was placed: a CD's cue sheet and tracks, or a DVD's image.
    pub roms: Vec<title_select_api::build::ExpectedRom>,
    /// A CD's cue sheet as kept in the store, or why it can't be read.
    pub cue: Option<Result<Vec<u8>, String>>,
}

/// How an Audit runs chdman to read a placed CHD: chdman, or why it can't be run; how long each run may take; and the
/// scratch directory it extracts into, a folder of its own for each CHD, removed once read.
#[derive(Debug, Clone)]
pub struct ChdAudit {
    pub chdman: Result<chd::Chdman, String>,
    pub limits: chd::Limits,
    pub tmp: std::path::PathBuf,
}

impl ChdAudit {
    /// Verifies the CHD at `path`, whose header is `header`, where it is with `chdman verify` (ADR 0026), in a scratch
    /// folder of its own that is removed once it is done: what makes a Disk Verified.
    pub fn verify(&self, path: &Path, header: &chd::Header) -> Result<(), chd::Unverified> {
        use chd::Unverified::Unchecked;
        static SCRATCH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let chdman = chd::Chdman {
            deadline: self.limits.for_bytes(header.logical_bytes),
            ..self
                .chdman
                .clone()
                .map_err(|why| Unchecked(format!("its CHD can't be verified: {why}")))?
        };
        let dir = self.tmp.join(format!(
            "verify-disk-{}-{}",
            std::process::id(),
            SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _scratch = read::ChdScratch(dir.clone());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| Unchecked(format!("{}: {e}", dir.display())))?;
        let input = std::path::absolute(path).map_err(|e| Unchecked(e.to_string()))?;
        chdman.verify_in_place(&dir, &input, &|| false)
    }

    /// chdman as [`chd::locate`] finds it without a configuration, with the default limits, its scratch directories in
    /// the system's temporary directory: what an Audit without a store runs.
    pub fn located() -> ChdAudit {
        ChdAudit::verifying(
            chd::locate(None),
            Default::default(),
            Default::default(),
            std::env::temp_dir(),
        )
    }

    /// Verifying Disks with `program`, as [`chd::locate`] found it, counting what it reads in `tally`, each run within
    /// `limits`, its scratch directories in `tmp`.
    pub fn verifying(
        program: Result<std::path::PathBuf, String>,
        tally: meter::Tally,
        limits: chd::Limits,
        tmp: std::path::PathBuf,
    ) -> ChdAudit {
        ChdAudit {
            chdman: program.map(|program| chd::Chdman {
                program,
                threads: None,
                deadline: Default::default(),
                tally,
            }),
            limits,
            tmp,
        }
    }
}

/// A file as `stat` sees it: what the change test compares. An earlier read of a file is reused only while every
/// field is as it was when the file was opened for that read. This is a heuristic: a file rewritten in place, with
/// its size kept and its times put back, would pass it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    /// Modification time, in nanoseconds since the Unix epoch.
    pub mtime_ns: i128,
    /// Status change time, in nanoseconds since the Unix epoch.
    pub ctime_ns: i128,
}

impl FileStamp {
    /// The stamp of the file at `path`, following symlinks; none where the platform has no inode or status change
    /// time, so that nothing is reused there.
    pub fn of(path: &Path) -> Option<FileStamp> {
        std::fs::metadata(path)
            .ok()
            .and_then(|m| FileStamp::from_metadata(&m))
    }

    #[cfg(unix)]
    pub fn from_metadata(m: &std::fs::Metadata) -> Option<FileStamp> {
        use std::os::unix::fs::MetadataExt;
        let ns = |s: i64, n: i64| s as i128 * 1_000_000_000 + n as i128;
        Some(FileStamp {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime_ns: ns(m.mtime(), m.mtime_nsec()),
            ctime_ns: ns(m.ctime(), m.ctime_nsec()),
        })
    }

    #[cfg(not(unix))]
    pub fn from_metadata(_: &std::fs::Metadata) -> Option<FileStamp> {
        None
    }
}

/// Every payload of a file, read in full from the file as `stamp` describes it, starting at `read_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadRead {
    pub stamp: FileStamp,
    pub read_at: jiff::Timestamp,
    /// Each payload's member name in a ZIP archive, or none for a loose file, in archive order.
    pub members: Vec<(Option<String>, Checksums)>,
    /// The Header Rule the payloads were hashed under. A read is reused only under the same rule.
    pub header_rule: Option<HeaderRule>,
    /// For a CHD read as a Disk, the combined SHA-1 `chdman verify` proved its data hashes to; it then has no
    /// `members`. A read is reused as a Disk's only when it is one.
    pub disk: Option<String>,
}

/// Earlier payload reads an Audit may reuse, and where it records its own. Both are called from the reader
/// threads, once per file.
pub trait PayloadReads: Sync {
    /// The last recorded read of the file at `path`, relative to the Source Set, if this Audit may reuse it. It is
    /// reused only if the file's stamp is still the one it records.
    fn prior(&self, path: &str) -> Option<PayloadRead>;
    /// Records a read this Audit made of every payload of the file at `path`. A file with any payload that could
    /// not be read is not recorded.
    fn record(&self, path: &str, read: &PayloadRead);
    /// Records the stamp the file at `path` had when this Audit could not read one of its payloads, so that what is
    /// done about it later can tell whether it changed since.
    fn record_unreadable(&self, _path: &str, _stamp: &FileStamp) {}
    /// Notes that this Audit reused the read `prior` returned for the file at `path`, whose stamp is still `stamp`.
    fn reused(&self, _path: &str, _stamp: &FileStamp) {}
    /// Notes the stamp the ZIP archive at `path` had when this Audit opened it to claim its members.
    fn claimed(&self, _path: &str, _stamp: &FileStamp) {}
}

/// Whether `name`, an entry anywhere in a Source Set, is the `.title-select` directory Title Select keeps there for
/// itself, which an Audit passes over with all it holds: the marker, kept files, and any file being written under a
/// temporary name.
pub fn is_own_entry(name: &str) -> bool {
    title_select_api::storage::is_reserved_name(name)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Audits the Source Set at `root` against the DAT Revision at `dat_path`, reading every payload, and verifying each
/// Disk's CHD with chdman as [`ChdAudit::located`] finds it.
pub fn audit(dat_path: &Path, root: &Path) -> Result<Report, Error> {
    let file = dat_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    audit_with(
        dat_path,
        root,
        &Options {
            file,
            confine: None,
            workers: ONE_WORKER_EACH,
            pools: None,
            progress: None,
            evidence: AuditEvidence::Full,
            reads: None,
            tally: Default::default(),
            placed: &[],
            disks: Some(&ChdAudit::located()),
        },
    )
}

/// What the walk found, in report order.
enum Item {
    Unreadable { rel: String, reason: String },
    File { rel: String },
}

/// What a walk of a Source Set found: its items in report order, and the files among them to read.
struct Walked {
    items: Vec<Item>,
    files: Vec<ToRead>,
}

/// Walks the Source Set at `root` as an Audit does, passing over Title Select's own entries and confined as
/// `confine` says. Stops, returning none, once `keep_going` says no.
fn walk(
    root: &Path,
    confine: Option<&Path>,
    keep_going: &dyn Fn() -> bool,
) -> Result<Option<Walked>, Error> {
    let rel = |p: &Path| {
        p.strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    };
    let mut items = Vec::new();
    let mut files: Vec<ToRead> = Vec::new();
    let mut walk = walkdir::WalkDir::new(root)
        .follow_links(true)
        .sort_by_file_name()
        .into_iter();
    while let Some(item) = walk.next() {
        if !keep_going() {
            return Ok(None);
        }
        let item = match item {
            Ok(item) => item,
            Err(err) if err.depth() == 0 => return Err(Error::SourceSet(err.to_string())),
            Err(err) => {
                let path = err.path().map(rel).unwrap_or_default();
                items.push(Item::Unreadable {
                    rel: path,
                    reason: err.to_string(),
                });
                continue;
            }
        };
        if item.depth() > 0 && is_own_entry(&item.file_name().to_string_lossy()) {
            if item.file_type().is_dir() {
                walk.skip_current_dir();
            }
            continue;
        }
        let mut path = item.path().to_path_buf();
        if let Some(confine) = confine {
            let refusal = match item.path().canonicalize() {
                Ok(real) if real.starts_with(confine) => {
                    path = real;
                    None
                }
                Ok(_) => Some("resolves outside the Root".to_owned()),
                Err(err) => Some(err.to_string()),
            };
            if let Some(reason) = refusal {
                if item.depth() == 0 {
                    return Err(Error::SourceSet(format!("{}: {reason}", root.display())));
                }
                if item.file_type().is_dir() {
                    walk.skip_current_dir();
                }
                items.push(Item::Unreadable {
                    rel: rel(item.path()),
                    reason,
                });
                continue;
            }
        }
        if !item.file_type().is_file() {
            continue;
        }
        let is_zip = item
            .path()
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
        let rel = rel(item.path());
        items.push(Item::File { rel: rel.clone() });
        files.push(ToRead {
            path,
            is_zip,
            rel,
            beneath: None,
            chd: None,
            disk: false,
        });
    }

    Ok(Some(Walked { items, files }))
}

/// What an Audit reads of the Placed Games in one Library Folder: each recorded file still there, in path order,
/// named by its path in the Source Set's report, with its stamp. One that is gone is left out; one that can't be
/// reached, is no longer a regular file or is a symbolic link is Unreadable. No symbolic link below the folder's Root
/// is followed: a folder that is one, or is in one, reads as Unreadable, never as the folder it leads to.
fn walk_placed(placed: &Placed) -> (Walked, Vec<Option<FileStamp>>) {
    let mut recorded: Vec<&String> = placed.files.iter().collect();
    recorded.sort();
    let gone = |e: &std::io::Error| {
        matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
        )
    };
    let base = placed.root;
    let mut items = Vec::new();
    let mut files = Vec::new();
    let mut stamps = Vec::new();
    for rel in recorded {
        let at = title_select_api::storage::placed_path(placed.id, rel);
        let path = placed
            .folder
            .split('/')
            .chain(rel.split('/'))
            .filter(|part| !part.is_empty())
            .fold(base.to_path_buf(), |path, part| path.join(part));
        let found = match read::open_beneath(base, &path) {
            Ok(file) => Ok((path, base, file.metadata().ok())),
            Err(e) if gone(&e) => continue,
            Err(e) => Err(e.to_string()),
        };
        match found {
            Err(reason) => items.push(Item::Unreadable { rel: at, reason }),
            Ok((path, base, metadata)) => {
                let is_zip = rel
                    .rsplit_once('.')
                    .is_some_and(|(_, e)| e.eq_ignore_ascii_case("zip"));
                let chd = placed.chds.iter().find(|c| c.rel == *rel).map(|c| {
                    Box::new(read::ChdRead {
                        placed: c.clone(),
                        audit: placed.chd.clone(),
                    })
                });
                items.push(Item::File { rel: at.clone() });
                files.push(ToRead {
                    path,
                    is_zip: is_zip && chd.is_none(),
                    rel: at,
                    beneath: Some(base.to_path_buf()),
                    chd,
                    disk: false,
                });
                stamps.push(metadata.and_then(|m| FileStamp::from_metadata(&m)));
            }
        }
    }
    (Walked { items, files }, stamps)
}

/// The recorded files of the Placed Games in `placed` that are still there, each statted as [`stat_walk`] does and
/// named by its path in the Source Set's report.
pub fn stat_placed(placed: &Placed) -> Vec<StatEntry> {
    let (Walked { items, .. }, stamps) = walk_placed(placed);
    let mut stamps = stamps.into_iter();
    items
        .into_iter()
        .map(|item| match item {
            Item::Unreadable { rel, .. } => StatEntry::Unreadable { rel },
            Item::File { rel } => StatEntry::File {
                rel,
                stamp: stamps.next().expect("one stamp per file"),
            },
        })
        .collect()
}

/// A file a stat walk found, with its stamp; none where it could not be statted or the platform has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatEntry {
    File {
        rel: String,
        stamp: Option<FileStamp>,
    },
    /// A path the walk could not read, or that resolves outside the Root.
    Unreadable { rel: String },
}

/// Walks the Source Set at `root` as an Audit does, stats every file it finds and reads no payload. Stops, returning
/// none, once `keep_going` says no; it is asked before each entry and each stat.
pub fn stat_walk(
    root: &Path,
    confine: Option<&Path>,
    keep_going: &dyn Fn() -> bool,
) -> Result<Option<Vec<StatEntry>>, Error> {
    if !root.is_dir() {
        return Err(Error::SourceSet(format!(
            "{}: not a directory",
            root.display()
        )));
    }
    let Some(Walked { items, files }) = walk(root, confine, keep_going)? else {
        return Ok(None);
    };
    let mut files = files.into_iter();
    let mut found = Vec::with_capacity(items.len());
    for item in items {
        if !keep_going() {
            return Ok(None);
        }
        found.push(match item {
            Item::Unreadable { rel, .. } => StatEntry::Unreadable { rel },
            Item::File { rel } => {
                let file = files.next().expect("one file per item");
                StatEntry::File {
                    rel,
                    stamp: FileStamp::of(&file.path),
                }
            }
        });
    }
    Ok(Some(found))
}

/// Like [`audit`], with the DAT Revision's filename, confinement, worker counts and progress given by `options`.
pub fn audit_with(dat_path: &Path, root: &Path, options: &Options) -> Result<Report, Error> {
    let started_at = jiff::Timestamp::now();
    if !root.is_dir() {
        return Err(Error::SourceSet(format!(
            "{}: not a directory",
            root.display()
        )));
    }
    let dat = Dat::read(dat_path)?;
    let Walked { items, mut files } =
        walk(root, options.confine, &|| true)?.expect("an audit's walk is never stopped");
    // A Source Set of a DAT that lists Disks keeps them as CHDs (ADR 0026), each read by its header.
    if dat.has_disks() {
        for file in &mut files {
            file.disk = !file.is_zip
                && file
                    .path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("chd"));
        }
    }
    let placed: Vec<Walked> = options.placed.iter().map(|p| walk_placed(p).0).collect();

    let header = dat.header();
    let sizes: HashSet<u64> = match header {
        Some(_) => dat.roms.iter().map(|r| r.size).collect(),
        None => HashSet::new(),
    };
    // A member whose size less the Header is a ROM's size is read, even when a ROM's size and CRC32 equal its raw
    // ones: a CRC32 match proves nothing of whether it holds a Header.
    let might_be_headered =
        |size: u64| header.is_some_and(|h| size > h.size() && sizes.contains(&(size - h.size())));
    let how = read::How {
        evidence: options.evidence,
        reads: options.reads,
        header,
        might_be_headered: &might_be_headered,
        tally: &options.tally,
        disks: options.disks,
    };
    // Progress counts every file read, the Source Set's own and its Placed Games' in each Library Folder.
    let total = files.len() + placed.iter().map(|w| w.files.len()).sum::<usize>();
    let progress_from = |before: usize| {
        move |p: Progress| {
            if let Some(progress) = options.progress {
                progress(Progress {
                    files_done: before + p.files_done,
                    files_total: total,
                });
            }
        }
    };
    let own = progress_from(0);
    let reads = read::read_files(
        &files,
        how,
        options.workers,
        options.pools,
        options.progress.map(|_| &own as &(dyn Fn(Progress) + Sync)),
    )
    .map_err(Error::Workers)?;
    let mut segments = vec![(items, reads.into_iter())];
    let mut before = files.len();
    for (walked, from) in placed.into_iter().zip(options.placed) {
        let progress = progress_from(before);
        let reads = read::read_files(
            &walked.files,
            read::How {
                tally: &from.tally,
                ..how
            },
            options.workers,
            from.pools,
            options
                .progress
                .map(|_| &progress as &(dyn Fn(Progress) + Sync)),
        )
        .map_err(Error::Workers)?;
        before += walked.files.len();
        segments.push((walked.items, reads.into_iter()));
    }
    let mut scan = Scan {
        holders: Holders::new(&dat),
        covered: vec![None; dat.roms.len()],
        elsewhere: vec![BTreeSet::new(); dat.roms.len()],
        chd: vec![None; dat.roms.len()],
        header_missing: vec![false; dat.games.len()],
        entries: Vec::new(),
        unnamed: Vec::new(),
        dat: &dat,
    };
    for (items, mut reads) in segments {
        for item in items {
            match item {
                Item::Unreadable { rel, reason } => scan.unreadable(rel, None, reason),
                Item::File { rel } => {
                    let FileRead {
                        members,
                        error,
                        origin,
                    } = reads.next().expect("one read per file");
                    for (member, known) in members {
                        match known {
                            Ok(known) => scan.record(rel.clone(), member, &known, origin),
                            Err(reason) => scan.unreadable(rel.clone(), member, reason),
                        }
                    }
                    if let Some(reason) = error {
                        scan.unreadable(rel, None, reason);
                    }
                }
            }
        }
    }
    scan.settle();
    let games = scan.games();
    let mut findings: BTreeMap<_, _> = Finding::ALL.iter().map(|f| (*f, 0)).collect();
    for e in &scan.entries {
        *findings.entry(e.finding).or_default() += 1;
    }
    let mut statuses: BTreeMap<_, _> = GameStatus::ALL.iter().map(|g| (*g, 0)).collect();
    let mut warnings = BTreeMap::new();
    for g in &games {
        *statuses.entry(g.status).or_default() += 1;
        for w in &g.warnings {
            *warnings.entry(*w).or_default() += 1;
        }
    }
    Ok(Report {
        schema: SCHEMA,
        dat_revision: DatRevision {
            sha256: dat.sha256.clone(),
            name: dat.name.clone(),
            version: dat.version.clone(),
            file: options.file.clone(),
            header_rule: dat.header_rule.as_deref().map(NamedHeaderRule::new),
        },
        source_set: root.display().to_string(),
        evidence: options.evidence,
        started_at,
        finished_at: jiff::Timestamp::now(),
        summary: Summary {
            findings,
            games: statuses,
            warnings,
        },
        entries: scan.entries,
        games,
    })
}

/// What an Audit has found so far.
struct Scan<'d> {
    dat: &'d Dat,
    holders: Holders<'d>,
    /// The strongest Evidence Level any Entry holding each ROM for its own Game has.
    covered: Vec<Option<EvidenceLevel>>,
    /// For each ROM, the other Games whose ZIP or folder holds an Entry whose payload is that ROM's (ADR 0017).
    elsewhere: Vec<BTreeSet<usize>>,
    /// For each Disk, the index in `entries` of the CHD [`Scan::count`] counted it present by.
    chd: Vec<Option<usize>>,
    /// Each Game a Matched Entry without the Header its emulators need counts towards.
    header_missing: Vec<bool>,
    entries: Vec<Entry>,
    /// The Entries in a Game's own ZIP or folder that may count for one of its ROMs by checksums alone, in walk
    /// order.
    unnamed: Vec<Unnamed>,
}

/// What an Entry tells of each ROM it counts for.
#[derive(Clone, Copy)]
struct Held {
    finding: Finding,
    level: EvidenceLevel,
    has_header: bool,
    /// The Entry's index in [`Scan::entries`].
    entry: usize,
}

/// An Entry in a Game's own ZIP or folder that none of the Game's ROMs is named for, and those of the Game's ROMs whose
/// checksums the Entry's payload has, in DAT order.
struct Unnamed {
    roms: Vec<usize>,
    held: Held,
}

impl Scan<'_> {
    fn unreadable(&mut self, path: String, member: Option<String>, reason: String) {
        self.entries.push(Entry {
            path,
            member,
            finding: Finding::Unreadable,
            roms: Vec::new(),
            named_rom: None,
            evidence: None,
            reason: Some(reason),
            disks: Vec::new(),
            parent_sha1: None,
        });
    }

    fn record(&mut self, path: String, member: Option<String>, known: &Known, origin: Origin) {
        match known {
            Known::Delta(parent) => {
                self.unreadable(path, member, needs_parent(self.dat, parent));
                if let Some(entry) = self.entries.last_mut() {
                    entry.parent_sha1 = Some(parent.clone());
                }
                return;
            }
            Known::NoDisk(why) => {
                self.entries.push(Entry {
                    path,
                    member,
                    finding: Finding::Unmatched,
                    roms: Vec::new(),
                    named_rom: None,
                    // No checksum of it was compared: it holds none a Disk is listed by.
                    evidence: None,
                    reason: Some(why.clone()),
                    disks: Vec::new(),
                    parent_sha1: None,
                });
                return;
            }
            _ => {}
        }
        let name = entry_name(&path, member.as_deref());
        let container = self.holders.container(&path, member.as_deref());
        // An Entry inside another Game's ZIP or folder makes a ROM Available Elsewhere, never present (ADR 0017). Its
        // ancestors' are the Game's own (ADR 0025): merged and split sets keep a family's shared files there.
        let elsewhere = |r: usize| {
            container.filter(|c| self.holders.holder(*c, self.dat.roms[r].game).is_none())
        };
        let (finding, payload, named) = classify(self.dat, name, known, |named| {
            self.holders.nearest(container, named.iter().copied())
        });
        // A file under a nodump ROM's name can't match it: no good dump is known (ADR 0025).
        let reason = named
            .filter(|r| {
                finding == Finding::Mismatched && self.dat.roms[*r].status == DumpStatus::NoDump
            })
            .map(|_| NOT_DUMPED.to_owned());
        let matching = matching_roms(self.dat, known);
        // A file whose name gives a nodump ROM ([`Holders::nodump_named`]) is that ROM's, which no file can match
        // (ADR 0025): it never counts by its checksums for another ROM, of the Game whose file holds it or of any
        // other in the family, as a repair leaves it where it is by default. It still counts for a ROM it is named
        // for and matches.
        let nodump_name = self.holders.nodump_named(container, name).is_some();
        // A Matched Entry's payload is the ROMs it is named for.
        let named_for = |r: &usize| finding == Finding::Matched && payload.contains(r);
        // In a family's ZIP or folder, a ROM counts by its name or, failing that, by its checksums under another
        // name, as MAME searches (ADR 0025), such as a clone's ROM that its parent lists under another name. In the
        // Game's own, an Entry counts only for the ROMs it is named for or, failing that, for one ROM with its
        // checksums that nothing else counts for, settled once every Entry is in ([`Scan::settle`]). A loose
        // file in no Game's counts towards the ROMs its Finding gives.
        let (container_roms, mut counted): (Vec<usize>, Vec<usize>) = match container {
            Some(c) => matching
                .iter()
                .copied()
                .filter(|r| elsewhere(*r).is_none() && (!nodump_name || named_for(r)))
                .partition(|r| self.dat.roms[*r].game == c),
            None => (Vec::new(), payload.clone()),
        };
        // Every ROM whose checksums the Entry's payload has, not only those it counts towards, so a track named for
        // its own Game still shows where another Game's copy is. Repair cannot yet source from those: it fills a
        // Game only from Entries that count towards it.
        let others: Vec<(usize, usize)> = matching
            .into_iter()
            .filter_map(|r| elsewhere(r).map(|g| (r, g)))
            .collect();
        for (r, g) in others {
            self.elsewhere[r].insert(g);
        }
        let disk = matches!(known, Known::Disk(_));
        let (level, header) = match known {
            Known::Payload(sums) => (EvidenceLevel::Payload, sums.header),
            Known::Disk(_) if origin != Origin::Claimed => (EvidenceLevel::Payload, None),
            Known::Claim { .. } | Known::Disk(_) => (EvidenceLevel::Claim, None),
            Known::Delta(_) | Known::NoDisk(_) => unreachable!("recorded above"),
        };
        let held = Held {
            finding,
            level,
            has_header: header.is_some(),
            entry: self.entries.len(),
        };
        let (by_name, by_checksums): (Vec<usize>, Vec<usize>) =
            container_roms.into_iter().partition(named_for);
        if by_name.is_empty() {
            if !by_checksums.is_empty() {
                self.unnamed.push(Unnamed {
                    roms: by_checksums,
                    held,
                });
            }
        } else {
            counted.extend(by_name);
        }
        for r in counted {
            self.count(r, held);
        }
        self.entries.push(Entry {
            path,
            member,
            finding,
            evidence: Some(Evidence {
                header: header.map(|bytes| Header { bytes }),
                ..evidence(self.dat, &payload, origin, disk)
            }),
            roms: payload.iter().map(|r| rom_ref(self.dat, *r)).collect(),
            named_rom: named.map(|r| rom_ref(self.dat, r)),
            reason,
            // Filled once every Entry is in ([`Scan::settle`]).
            disks: Vec::new(),
            parent_sha1: None,
        });
    }

    /// Counts the ROM `r` as present, and its Game as lacking a Header when `held` is a Matched Entry without one
    /// that the DAT's emulators need.
    fn count(&mut self, r: usize, held: Held) {
        if held.finding == Finding::Matched
            && !held.has_header
            && self.dat.header().is_some_and(HeaderRule::needed_to_play)
        {
            self.header_missing[self.dat.roms[r].game] = true;
        }
        if self.covered[r] != Some(EvidenceLevel::Payload) {
            // A Disk names the CHD it counts for, the first with the strongest Evidence Level.
            if self.dat.roms[r].disk && self.covered[r] != Some(held.level) {
                self.chd[r] = Some(held.entry);
            }
            self.covered[r] = Some(held.level);
        }
    }

    /// Counts each Entry in a Game's own ZIP or folder that none of its ROMs is named for towards the first of those
    /// ROMs with its checksums that nothing has counted for yet, once every other Entry is in, so that one file never
    /// stands in for several identical ROMs, such as a disc's identical tracks. A needed ROM comes before an optional
    /// one, which never lowers the Game Status (ADR 0025). A ROM another Entry counts for only by Claim Evidence stays
    /// at Claim. Then names, in each CHD's Entry, the Disks it counts for.
    fn settle(&mut self) {
        for mut u in std::mem::take(&mut self.unnamed) {
            u.roms.sort_by_key(|r| !self.dat.roms[*r].needed());
            if let Some(r) = u.roms.into_iter().find(|r| self.covered[*r].is_none()) {
                self.count(r, u.held);
            }
        }
        for (r, e) in self.chd.iter().enumerate() {
            if let Some(e) = e {
                self.entries[*e].disks.push(rom_ref(self.dat, r));
            }
        }
    }

    fn games(&self) -> Vec<Game> {
        let mut roms_of: Vec<Vec<usize>> = vec![Vec::new(); self.dat.games.len()];
        for (r, rom) in self.dat.roms.iter().enumerate() {
            roms_of[rom.game].push(r);
        }
        let mut games: Vec<Game> = self
            .dat
            .games
            .iter()
            .zip(roms_of)
            .zip(&self.header_missing)
            .map(|((name, roms), header_missing)| {
                let missing_roms: Vec<String> = roms
                    .iter()
                    .filter(|r| self.covered[**r].is_none())
                    .map(|r| self.dat.roms[*r].name.clone())
                    .collect();
                let available_elsewhere: Vec<AvailableElsewhere> = roms
                    .iter()
                    .filter(|r| self.covered[**r].is_none() && !self.elsewhere[**r].is_empty())
                    .map(|r| AvailableElsewhere {
                        rom: self.dat.roms[*r].name.clone(),
                        games: self.elsewhere[*r]
                            .iter()
                            .map(|g| self.dat.games[*g].clone())
                            .collect(),
                    })
                    .collect();
                let claimed_roms: Vec<String> = roms
                    .iter()
                    .filter(|r| self.covered[**r] == Some(EvidenceLevel::Claim))
                    .map(|r| self.dat.roms[*r].name.clone())
                    .collect();
                // A ROM it needn't have, optional or nodump, never lowers its Game Status (ADR 0025).
                let mut needed = roms
                    .iter()
                    .filter(|r| self.dat.roms[**r].needed())
                    .peekable();
                let status = if needed.peek().is_none() {
                    GameStatus::NothingToCheck
                } else if needed.all(|r| self.covered[*r].is_some()) {
                    GameStatus::Complete
                } else if missing_roms.len() == roms.len() {
                    GameStatus::Missing
                } else {
                    GameStatus::Incomplete
                };
                let names_of = |of: fn(&dat::Rom) -> bool| -> Vec<String> {
                    roms.iter()
                        .map(|r| &self.dat.roms[*r])
                        .filter(|r| of(r))
                        .map(|r| r.name.clone())
                        .collect()
                };
                Game {
                    name: name.clone(),
                    status,
                    missing_roms,
                    claimed_roms,
                    not_dumped: names_of(|r| r.status == DumpStatus::NoDump),
                    bad_dumps: names_of(|r| r.status == DumpStatus::BadDump),
                    optional_roms: names_of(|r| r.optional),
                    warnings: header_missing
                        .then_some(GameWarning::HeaderMissing)
                        .into_iter()
                        .collect(),
                    available_elsewhere,
                    // Filled below, once every Game's status is known.
                    dependencies: Vec::new(),
                    disks: names_of(|r| r.disk),
                }
            })
            .collect();
        // A Dependency's Game Status is its own, and changes none of the Games that need it.
        let statuses: Vec<GameStatus> = games.iter().map(|g| g.status).collect();
        for (g, game) in games.iter_mut().enumerate() {
            game.dependencies = self
                .dat
                .dependencies(g)
                .into_iter()
                .map(|d| Dependency {
                    game: self.dat.games[d].clone(),
                    status: statuses[d],
                })
                .collect();
        }
        games
    }
}

/// Why a delta CHD whose parent's combined SHA-1 is `parent` can't be read, naming the Disk of `dat` that parent is.
pub fn needs_parent(dat: &Dat, parent: &str) -> String {
    let named = dat.disks_with_sha1(parent).first().map(|r| {
        let rom = &dat.roms[*r];
        format!("{} of {}", rom.name, dat.games[rom.game])
    });
    match named {
        Some(named) => format!(
            "{DELTA_CHD}, which can't be read without its parent CHD, {named} (SHA-1 {parent}); replace it \
             with a self-contained CHD"
        ),
        None => format!(
            "{DELTA_CHD}, which can't be read without its parent CHD, SHA-1 {parent}, which no disk of the \
             DAT is; replace it with a self-contained CHD"
        ),
    }
}

/// For a payload match, size plus each checksum that every matched ROM supplies, since every supplied
/// checksum was compared and agreed. Without one, only the size-and-CRC32 lookup that found no candidate. A claim
/// compares only size and CRC32. A CHD read as a Disk, `disk`, is compared by its combined SHA-1 alone.
fn evidence(dat: &Dat, roms: &[usize], origin: Origin, disk: bool) -> Evidence {
    let (level, read_at, reused) = match origin {
        Origin::Read(at) => (EvidenceLevel::Payload, Some(at), None),
        Origin::Reused(at) => (
            EvidenceLevel::Payload,
            Some(at),
            Some(Reuse::UnchangedFileHeuristic),
        ),
        Origin::Claimed => (EvidenceLevel::Claim, None, None),
    };
    let algorithms = if disk {
        vec!["sha1".into()]
    } else if roms.is_empty() || level == EvidenceLevel::Claim {
        vec!["size".into(), "crc32".into()]
    } else {
        let all_supply = |has: fn(&dat::Rom) -> bool| roms.iter().all(|r| has(&dat.roms[*r]));
        let mut algorithms = vec!["size".into()];
        for (name, has) in [
            ("crc32", (|r| r.crc.is_some()) as fn(&dat::Rom) -> bool),
            ("md5", |r| r.md5.is_some()),
            ("sha1", |r| r.sha1.is_some()),
            ("sha256", |r| r.sha256.is_some()),
        ] {
            if all_supply(has) {
                algorithms.push(name.into());
            }
        }
        algorithms
    };
    Evidence {
        level,
        algorithms,
        read_at,
        reused,
        header: None,
    }
}

fn rom_ref(dat: &Dat, r: usize) -> RomRef {
    RomRef {
        game: dat.games[dat.roms[r].game].clone(),
        name: dat.roms[r].name.clone(),
    }
}

/// The name of the Entry at `path`, `member` of it in a ZIP: its file name, less any folders.
pub fn entry_name<'a>(path: &'a str, member: Option<&'a str>) -> &'a str {
    let full = member.unwrap_or(path);
    full.rsplit(['/', '\\']).next().unwrap_or(full)
}

/// Which Game's ZIP or folder holds an Entry, and what that Game is to the Game of each ROM the Entry may be: the one
/// place an Audit and a Repair tell whose file an Entry is in.
pub struct Holders<'d> {
    dat: &'d Dat,
    /// The parent each Game is a clone of, by the Game's index.
    parents: Vec<Option<usize>>,
    /// The Games whose ZIPs and folders count as each Game's own, by the Game's index ([`Dat::ancestors`]).
    ancestors: Vec<Vec<usize>>,
}

impl<'d> Holders<'d> {
    pub fn new(dat: &'d Dat) -> Self {
        Holders {
            dat,
            parents: (dat.cloneof.iter())
                .map(|p| p.as_deref().and_then(|p| dat.game_index(p)))
                .collect(),
            ancestors: (0..dat.games.len()).map(|g| dat.ancestors(g)).collect(),
        }
    }

    /// The Game whose ZIP or folder holds the Entry at `path` (relative to the Source Set's root, or a Placed Game's
    /// in its Library Folder), `member` of it in a ZIP: the nearest of the ZIP, by its name less `.zip`, and the
    /// folders above it or the loose file that is named for a Game. None when none is.
    pub fn container(&self, path: &str, member: Option<&str>) -> Option<usize> {
        // A Placed Game's file is held as it is in its Library Folder.
        let path = title_select_api::storage::split_placed(path).map_or(path, |(_, rel)| rel);
        let mut parts = path.split(['/', '\\']).rev();
        let file = parts.next()?;
        let zip = member.and_then(|_| {
            let (stem, ext) = file.rsplit_once('.')?;
            ext.eq_ignore_ascii_case("zip").then_some(stem)
        });
        zip.into_iter()
            .chain(parts)
            .find_map(|name| self.dat.game_index(name))
    }

    /// What the Game `container`, whose ZIP or folder holds an Entry, is to `game`; none when it is none of that
    /// Game's ancestors ([`Dat::ancestors`]).
    fn holder(&self, container: usize, game: usize) -> Option<Holder> {
        match container {
            c if c == game => Some(Holder::Own),
            c if Some(c) == self.parents[game] => Some(Holder::Parent),
            c if self.ancestors[game].contains(&c) => Some(Holder::Ancestor),
            _ => None,
        }
    }

    /// The nodump ROM whose name an Entry named `name` is under, when [`Holders::nearest`] gives one of the ROMs with
    /// that name for the Game `container`, whose ZIP or folder holds it. No file can match that ROM (ADR 0025).
    pub fn nodump_named(&self, container: Option<usize>, name: &str) -> Option<usize> {
        self.nearest(container, self.dat.roms_named(name).iter().copied())
            .filter(|r| self.dat.roms[*r].status == DumpStatus::NoDump)
    }

    /// Of `roms`, one of the Game `container`, or else one of a clone of it, or else one of a Game it is another
    /// ancestor of, such as a Game whose BIOS it is; of several, a nodump one first, as no file can match it, then the
    /// first in the order given. None when none is any of those, or no Game's file holds the Entry.
    pub fn nearest(
        &self,
        container: Option<usize>,
        roms: impl Iterator<Item = usize>,
    ) -> Option<usize> {
        let container = container?;
        roms.filter_map(|r| {
            let rom = &self.dat.roms[r];
            let holder = self.holder(container, rom.game)?;
            // False, a nodump ROM, sorts first.
            let dumped = rom.status != DumpStatus::NoDump;
            Some(((holder, dumped), r))
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, r)| r)
    }
}

/// What the Game whose ZIP or folder holds an Entry is to a ROM's Game. Its order is the order the name of a
/// Mismatched Entry is given a ROM in: the holding Game's own ROM first, whatever else may count that file as its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Holder {
    /// That Game.
    Own,
    /// Its parent: merged and split sets keep a clone's files in its parent's ZIP or folder, so they count as the
    /// clone's own.
    Parent,
    /// Another of its ancestors, such as its BIOS or its parent's: split sets keep a family's shared files there, so
    /// they count as its own (ADR 0025).
    Ancestor,
}

/// Decides an Entry's Finding by payload before name (ADR 0002). Returns the Finding, the ROMs the payload
/// matches, and the ROM the name gives when that differs from the payload's. For a Mismatched Entry, that is the
/// ROM `nearest` picks of those with its name ([`Holders::nearest`]), or else the first of them in the DAT.
fn classify(
    dat: &Dat,
    name: &str,
    known: &Known,
    nearest: impl Fn(&[usize]) -> Option<usize>,
) -> (Finding, Vec<usize>, Option<usize>) {
    let payload = matching_roms(dat, known);
    let named = dat.roms_named(name);
    // Every ROM with this name and payload, so an identical ROM shared by several Games counts for each.
    let both: Vec<usize> = named
        .iter()
        .copied()
        .filter(|r| payload.contains(r))
        .collect();
    if !both.is_empty() {
        return (Finding::Matched, both, None);
    }
    match (payload.is_empty(), named.first()) {
        (false, None) => (Finding::Misnamed, payload, None),
        (false, Some(n)) => (Finding::Conflicting, payload, Some(*n)),
        (true, Some(n)) => {
            let named = nearest(named).unwrap_or(*n);
            (Finding::Mismatched, payload, Some(named))
        }
        (true, None) => (Finding::Unmatched, payload, None),
    }
}

/// The ROMs whose every supplied checksum equals the payload's; for a claim, the ROMs whose size and CRC32 equal
/// the claimed ones.
fn matching_roms(dat: &Dat, known: &Known) -> Vec<usize> {
    match known {
        Known::Payload(sums) => dat
            .candidates(sums.size, &sums.crc)
            .filter(|r| sums.matches(&dat.roms[*r]))
            .collect(),
        Known::Claim { size, crc } => dat
            .candidates(*size, crc)
            .filter(|r| dat.roms[*r].crc.as_deref() == Some(crc.as_str()))
            .collect(),
        Known::Disk(sha1) => dat.disks_with_sha1(sha1).to_vec(),
        Known::Delta(_) | Known::NoDisk(_) => Vec::new(),
    }
}
