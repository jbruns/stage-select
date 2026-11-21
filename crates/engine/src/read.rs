//! Reading payloads with bounded workers: `readers` threads open files and read (and decompress) their payloads,
//! handing each payload's bytes in chunks to one of `hashers` threads that checksums it. A file whose earlier read
//! passes the change test is not read again, and a quick Audit claims a ZIP archive's members from its directory.
//!
//! [`Pools`] say how many work at each moment: each reader takes a permit of its Root's readers pool before each file,
//! and each hash worker one of the Host's hash workers pool before each payload (ADR 0022). Other Jobs share the
//! pools, whose size is the pace's share of each Ceiling at that moment. A Job starts its threads as the pools let
//! them work, so it grows when they do and its threads wait when they shrink (see [`Crew`]).
//!
//! A reader holds its permit while it waits for a hash worker; a hash worker takes its permit only once it has a
//! payload, and then waits only for that payload's reader, which already holds its own. So no worker holding a hash
//! worker permit ever waits for a reader permit, and Jobs sharing both pools can't deadlock. Jobs that read a Root
//! without this pipeline take the same Root's permits through their [`Tally`] (see [`crate::meter`]), one at a time.

use std::cell::Cell;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvError, SyncSender, sync_channel};
use std::sync::{Mutex, PoisonError};

use crate::meter::{JobMeter, ReaderPermit, Reading, Switched, Tally};
use crate::payload::{Checksums, Hasher};
use crate::pool::Pool;
use crate::{AuditEvidence, FileStamp, HeaderRule, PayloadRead, PayloadReads, Progress, Workers};

const CHUNK: usize = 1 << 16;
/// Chunks a reader may get ahead of its hash worker.
const CHUNKS_IN_FLIGHT: usize = 4;

/// What an Audit knows of one payload's checksums.
pub enum Known {
    /// Every checksum, computed from its bytes.
    Payload(Checksums),
    /// Its size and CRC32 as an archive's directory gives them.
    Claim { size: u64, crc: String },
    /// A CHD's combined SHA-1, as its header records it: what a Disk is listed by. Its Evidence is the file's
    /// [`Origin`]: claimed from the header, or proven by `chdman verify`.
    Disk(String),
    /// A delta CHD, which can't be read without the parent CHD whose combined SHA-1 this is.
    Delta(String),
    /// A file read as a Disk that can match none, and why: a CHD older than version 5, or no CHD.
    NoDisk(String),
}

/// Where a file's checksums came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Its payloads were read, starting at this time.
    Read(jiff::Timestamp),
    /// An earlier read, made at this time, was reused under the change test.
    Reused(jiff::Timestamp),
    /// Its members were claimed from its directory.
    Claimed,
}

/// The Entries of one file, in archive order, and an error about the file as a whole. A member that fails to read
/// has its own error, and the rest of the archive is still read; an error about the archive ends it.
pub struct FileRead {
    pub members: Vec<(Option<String>, Result<Known, String>)>,
    pub error: Option<String>,
    pub origin: Origin,
}

/// A file to read: where it is, whether it is a ZIP archive, and its path relative to the Source Set.
pub struct ToRead {
    pub path: PathBuf,
    pub is_zip: bool,
    pub rel: String,
    /// The folder below which no symbolic link is followed to reach `path`, when there is one: a Library Folder
    /// holding Placed Games, whose files are read only where they were placed.
    pub beneath: Option<PathBuf>,
    /// For a Placed Game's CHD, what it holds and how chdman reads it.
    pub chd: Option<Box<ChdRead>>,
    /// Whether it is a CHD read as a Disk (ADR 0026): a `.chd` file of a Source Set whose DAT lists Disks.
    pub disk: bool,
}

/// A placed CHD to read, and how.
pub struct ChdRead {
    pub placed: crate::PlacedChd,
    pub audit: crate::ChdAudit,
}

impl ToRead {
    /// Opens the file to read it, following no symbolic link below [`ToRead::beneath`].
    pub fn open(&self) -> std::io::Result<File> {
        match &self.beneath {
            None => File::open(&self.path),
            Some(base) => open_beneath(base, &self.path),
        }
    }

    /// The file's stamp as it is now; none where it can't be opened or the platform has none.
    fn stamp(&self) -> Option<FileStamp> {
        match &self.beneath {
            None => FileStamp::of(&self.path),
            Some(_) => FileStamp::from_metadata(&self.open().ok()?.metadata().ok()?),
        }
    }
}

/// Opens the regular file at `path`, in the folder `base`, following no symbolic link below `base`: one found on the
/// way, or at `path` itself, is an error, never followed.
#[cfg(unix)]
pub fn open_beneath(base: &Path, path: &Path) -> std::io::Result<File> {
    use rustix::fs::{CWD, Mode, OFlags, openat};
    use std::path::Component;
    let other = |why: &str| std::io::Error::other(why.to_owned());
    let link = |e: rustix::io::Errno| match e {
        rustix::io::Errno::LOOP => {
            other("it is a symbolic link, which is never followed to a placed game's file")
        }
        e => e.into(),
    };
    let rel = path
        .strip_prefix(base)
        .map_err(|_| other("it is not in its folder"))?;
    let dir = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    let mut at = openat(CWD, base, dir, Mode::empty())?;
    let mut names = rel.components().peekable();
    while let Some(component) = names.next() {
        let Component::Normal(name) = component else {
            return Err(other("its path is not a plain one"));
        };
        if names.peek().is_some() {
            at = match openat(&at, name, dir | OFlags::NOFOLLOW, Mode::empty()) {
                Ok(next) => next,
                // A folder that is a symbolic link is refused as one, whatever the platform calls the refusal.
                Err(e) => {
                    let found =
                        rustix::fs::statat(&at, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW);
                    return Err(match found {
                        Ok(st) if rustix::fs::FileType::from_raw_mode(st.st_mode).is_symlink() => {
                            other(
                                "a folder of its path is a symbolic link, which is never followed to a placed \
                                 game's file",
                            )
                        }
                        _ => link(e),
                    });
                }
            };
            continue;
        }
        // Not blocking, so a FIFO put in its place is refused below rather than waited on.
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let file = File::from(openat(&at, name, flags, Mode::empty()).map_err(link)?);
        if !file.metadata()?.is_file() {
            return Err(other("it is no longer a regular file"));
        }
        return Ok(file);
    }
    Err(other("its path names no file"))
}

#[cfg(not(unix))]
pub fn open_beneath(_: &Path, _: &Path) -> std::io::Result<File> {
    Err(std::io::Error::other(
        "a placed game's file can't be read safely on this platform",
    ))
}

/// How each file is read.
#[derive(Clone, Copy)]
pub struct How<'a> {
    pub evidence: AuditEvidence,
    pub reads: Option<&'a dyn PayloadReads>,
    /// The DAT Revision's Header Rule: each payload is hashed without the Header it finds.
    pub header: Option<HeaderRule>,
    /// Whether a ZIP member of this claimed size might hold a Header the DAT leaves out. Its claim covers any
    /// Header, so it cannot say which ROM the member is; an archive holding one is read rather than claimed.
    pub might_be_headered: &'a (dyn Fn(u64) -> bool + Sync),
    /// Where each payload byte read is counted, and its Job's readers and hash workers at work.
    pub tally: &'a Tally,
    /// How chdman verifies a Disk's CHD; none where there is none to run.
    pub disks: Option<&'a crate::ChdAudit>,
}

/// One payload for a hash worker: its chunks, the Header Rule to hash them under, and where to send its checksums.
struct HashJob {
    chunks: Receiver<Vec<u8>>,
    header: Option<HeaderRule>,
    done: SyncSender<Checksums>,
}

/// Hands a payload to a free hash worker, waiting for one: false if none is running.
type Hand<'h> = &'h dyn Fn(HashJob) -> bool;

/// The pools a Job's workers share with every other Job: the payload readers of the Root it reads, and the Host's
/// hash workers.
#[derive(Clone, Copy)]
pub struct Pools<'a> {
    pub readers: &'a Pool,
    pub hashers: &'a Pool,
}

/// Reads every file in `files`, returning one [`FileRead`] per file in the same order. `pools` say how many readers
/// and hash workers may work at each moment; without them, `workers` does. Fails, having stopped every worker it
/// started, only if the system couldn't start a reader or a hash worker when the Job had none.
pub fn read_files(
    files: &[ToRead],
    how: How,
    workers: Workers,
    pools: Option<Pools>,
    progress: Option<&(dyn Fn(Progress) + Sync)>,
) -> Result<Vec<FileRead>, String> {
    read_counting_threads(files, how, workers, pools, progress, &|_| false).map(|(read, _)| read)
}

/// A kind of worker a Job starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Worker {
    Reader,
    Hasher,
}

/// Whether to refuse to start a worker of this kind, as the system would if it couldn't create the thread: for tests.
type Refuse<'a> = &'a (dyn Fn(Worker) -> bool + Sync);

/// The threads a Job's read started: readers and hash workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Started {
    readers: usize,
    hashers: usize,
}

/// A Job's readers and hash workers, and the work they share. Threads start as their pools let more of them work,
/// rather than all at once: a reader that takes a permit and finds a file left, the pool with room, and none of the
/// Job's readers waiting, starts another, and a reader that hands over a payload with none of the Job's hash workers
/// free and their pool with room starts one. So a Ceiling or the pace raised while the Job runs reaches it as its
/// workers start their next file, and the threads it started stay, waiting for a permit, when the pools shrink.
///
/// A pool's occupancy counts only the workers holding a permit, so each start is first reserved in the Job's count of
/// threads of its kind, by compare-and-swap against the pool's size at that moment and the Job's files. Threads
/// starting, or waiting for a permit, are counted, and racing starts can't take the Job past either bound: it never
/// has more readers, or more hash workers, than its pool allowed when each started or than it has files. Each start
/// costs a few atomic operations; a file costs nothing more.
///
/// A thread the system can't create gives its reservation back, and the Job carries on with the workers it has. Only
/// its first hash worker can leave it with none of a kind, a reader always having started it: then the Job stops,
/// with the reason. Its hash channel is closed, so a reader waiting to hand over a payload is let go, and readers
/// waiting for a permit give up their wait.
struct Crew<'a> {
    files: &'a [ToRead],
    how: How<'a>,
    pools: Pools<'a>,
    progress: Option<&'a (dyn Fn(Progress) + Sync)>,
    next: AtomicUsize,
    done: AtomicUsize,
    results: Mutex<Vec<Option<FileRead>>>,
    /// Taken, and so closed, when the Job stops.
    queue: Mutex<Option<Receiver<HashJob>>>,
    /// Readers started, or reserved to start.
    readers: AtomicUsize,
    /// Readers started that hold no permit: waiting for one, or about to.
    readers_waiting: AtomicUsize,
    /// Hash workers started, or reserved to start.
    hashers: AtomicUsize,
    /// Hash workers started that hold no payload.
    hashers_free: AtomicUsize,
    /// Why the Job stopped before every file was read.
    stopped: Mutex<Option<String>>,
    stopping: AtomicBool,
    refuse: Refuse<'a>,
}

impl<'a> Crew<'a> {
    fn has_room(pool: &Pool) -> bool {
        pool.taken() < pool.limit()
    }

    /// Starts a thread running `f`, unless the system can't create it.
    fn spawn<'s, 'e>(
        &'s self,
        s: &'s std::thread::Scope<'s, 'e>,
        kind: Worker,
        f: impl FnOnce() + Send + 's,
    ) -> Result<(), String> {
        if (self.refuse)(kind) {
            return Err("its thread was refused".into());
        }
        std::thread::Builder::new()
            .spawn_scoped(s, f)
            .map(drop)
            .map_err(|e| e.to_string())
    }

    /// Stops the Job's reads, with the reason: closes its hash channel, letting go every reader waiting to hand over
    /// a payload, and makes each reader stop at its next file or wait.
    fn stop(&self, why: String) {
        self.stopped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(why);
        self.stopping.store(true, Ordering::SeqCst);
        drop(
            self.queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }

    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Counts one more thread in `started` if that keeps it within `bound`.
    fn reserve(started: &AtomicUsize, bound: usize) -> bool {
        started
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < bound).then_some(n + 1)
            })
            .is_ok()
    }

    /// Starts another reader, counted as waiting until it takes a permit, if none of the Job's readers is waiting
    /// and the pool and the Job's files leave room for it.
    fn start_reader_if_room<'s, 'e>(
        &'s self,
        s: &'s std::thread::Scope<'s, 'e>,
        jobs: &SyncSender<HashJob>,
    ) {
        let pool = self.pools.readers;
        if !Crew::has_room(pool)
            || self
                .readers_waiting
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return;
        }
        if !Crew::reserve(&self.readers, pool.limit().min(self.files.len())) {
            self.readers_waiting.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        let jobs = jobs.clone();
        if self
            .spawn(s, Worker::Reader, move || self.read(s, jobs))
            .is_err()
        {
            // The reader starting it reads on.
            self.readers.fetch_sub(1, Ordering::SeqCst);
            self.readers_waiting.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn read<'s, 'e>(&'s self, s: &'s std::thread::Scope<'s, 'e>, jobs: SyncSender<HashJob>) {
        let files = self.files;
        let all_read = || self.next.load(Ordering::SeqCst) >= files.len() || self.is_stopping();
        loop {
            // A reader with no file left to read doesn't queue for a permit another Job could use.
            let permit = if all_read() {
                None
            } else {
                self.pools.readers.enter_unless(all_read)
            };
            self.readers_waiting.fetch_sub(1, Ordering::SeqCst);
            let Some(_permit) = permit else { break };
            // So that the file's reads don't ask the Root's pool for a second permit.
            let _held = ReaderPermit::held();
            let i = self.next.fetch_add(1, Ordering::SeqCst);
            let Some(file) = files.get(i) else { break };
            if i + 1 < files.len() {
                self.start_reader_if_room(s, &jobs);
            }
            let hand = |job: HashJob| {
                // The first starts however full the pool is: a payload handed over with none running would wait for
                // ever.
                if self.hashers.load(Ordering::SeqCst) == 0 || Crew::has_room(self.pools.hashers) {
                    self.start_hasher_unless_one_is_free(s);
                }
                jobs.send(job).is_ok()
            };
            let _active = self.how.tally.job().map(JobMeter::reading);
            let read = read_or_reuse(file, self.how, &hand);
            self.results.lock().unwrap_or_else(PoisonError::into_inner)[i] = Some(read);
            let files_done = self.done.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some(progress) = self.progress {
                progress(Progress {
                    files_done,
                    files_total: files.len(),
                });
            }
            self.readers_waiting.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Starts a hash worker, counted as free until it takes a payload, unless one is free already or the pool and
    /// the Job's files leave no room for it. The Job's first always starts.
    fn start_hasher_unless_one_is_free<'s, 'e>(&'s self, s: &'s std::thread::Scope<'s, 'e>) {
        if self
            .hashers_free
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let bound = self.pools.hashers.limit().min(self.files.len()).max(1);
        if !Crew::reserve(&self.hashers, bound) {
            self.hashers_free.fetch_sub(1, Ordering::SeqCst);
            return;
        }
        if let Err(why) = self.spawn(s, Worker::Hasher, move || self.hash()) {
            // The free one claimed is still counted, so no other reader starts one meanwhile: with none started, no
            // hash worker holds the channel, and closing it lets go every reader waiting to hand over a payload.
            if self.hashers.fetch_sub(1, Ordering::SeqCst) == 1 {
                self.stop(format!("the system couldn't start a hash worker: {why}"));
            }
            self.hashers_free.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn hash(&self) {
        loop {
            let job = match &*self.queue.lock().unwrap_or_else(PoisonError::into_inner) {
                Some(queue) => queue.recv(),
                None => Err(RecvError),
            };
            self.hashers_free.fetch_sub(1, Ordering::SeqCst);
            // The hash workers stop once every reader has dropped its sender.
            let Ok(job) = job else { break };
            let sums = {
                // Taken only with a payload in hand: see the module's note on deadlock.
                let _permit = self.pools.hashers.enter();
                let _active = self.how.tally.job().map(JobMeter::hashing);
                let mut hasher = Hasher::with_header(job.header);
                for chunk in job.chunks {
                    hasher.update(&chunk);
                }
                hasher.finish()
            };
            // Free before its reader hears, so that its next payload doesn't start another hash worker.
            self.hashers_free.fetch_add(1, Ordering::SeqCst);
            let _ = job.done.send(sums);
        }
    }
}

fn read_counting_threads(
    files: &[ToRead],
    how: How,
    workers: Workers,
    pools: Option<Pools>,
    progress: Option<&(dyn Fn(Progress) + Sync)>,
    refuse: Refuse,
) -> Result<(Vec<FileRead>, Started), String> {
    if files.is_empty() {
        let none = Started {
            readers: 0,
            hashers: 0,
        };
        return Ok((Vec::new(), none));
    }
    let (own_readers, own_hashers) = (Pool::new(workers.readers), Pool::new(workers.hashers));
    let pools = pools.unwrap_or(Pools {
        readers: &own_readers,
        hashers: &own_hashers,
    });
    let (jobs, queue) = sync_channel::<HashJob>(0);
    let crew = Crew {
        files,
        how,
        pools,
        progress,
        next: AtomicUsize::new(0),
        done: AtomicUsize::new(0),
        results: Mutex::new(files.iter().map(|_| None).collect()),
        queue: Mutex::new(Some(queue)),
        readers: AtomicUsize::new(1),
        readers_waiting: AtomicUsize::new(1),
        hashers: AtomicUsize::new(0),
        hashers_free: AtomicUsize::new(0),
        stopped: Mutex::new(None),
        stopping: AtomicBool::new(false),
        refuse,
    };
    std::thread::scope(|s| {
        let crew = &crew;
        if let Err(why) = crew.spawn(s, Worker::Reader, move || crew.read(s, jobs)) {
            crew.stop(format!("the system couldn't start a reader: {why}"));
        }
    });
    if let Some(why) = crew
        .stopped
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
    {
        return Err(why);
    }
    let started = Started {
        readers: crew.readers.into_inner(),
        hashers: crew.hashers.into_inner(),
    };
    let results = crew
        .results
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .into_iter()
        .map(|r| r.expect("every file is read"))
        .collect();
    Ok((results, started))
}

/// Reuses the file's earlier read if it passes the change test; else claims a ZIP archive's members in a quick
/// Audit; else reads its payloads, recording the read when every one of them was read.
fn read_or_reuse(file: &ToRead, how: How, jobs: Hand) -> FileRead {
    if let Some(chd) = &file.chd {
        return read_chd(file, chd, how, jobs);
    }
    if file.disk {
        return read_disk(file, how);
    }
    if let Some(reads) = how.reads {
        let now = file.stamp();
        if let (Some(now), Some(prior)) = (now, reads.prior(&file.rel))
            && prior.stamp == now
            && prior.header_rule == how.header
            && prior.disk.is_none()
        {
            reads.reused(&file.rel, &now);
            return FileRead {
                members: prior
                    .members
                    .into_iter()
                    .map(|(m, sums)| (m, Ok(Known::Payload(sums))))
                    .collect(),
                error: None,
                origin: Origin::Reused(prior.read_at),
            };
        }
    }
    if how.evidence == AuditEvidence::Quick && file.is_zip {
        let stamp = file.stamp();
        let claimed = claim_file(file);
        let headered = how.header.is_some()
            && claimed.members.iter().any(|(_, known)| {
                matches!(known, Ok(Known::Claim { size, .. }) if (how.might_be_headered)(*size))
            });
        if !headered {
            if let (Some(reads), Some(stamp)) = (how.reads, stamp) {
                reads.claimed(&file.rel, &stamp);
            }
            return claimed;
        }
    }
    let read_at = jiff::Timestamp::now();
    let (read, stamp) = read_file(file, jobs, read_at, how);
    how.tally.file_read();
    if let (Some(reads), Some(stamp)) = (how.reads, stamp) {
        let members: Option<Vec<_>> = read
            .members
            .iter()
            .map(|(m, sums)| match sums {
                Ok(Known::Payload(sums)) => Some((m.clone(), sums.clone())),
                _ => None,
            })
            .collect();
        match members {
            Some(members) if read.error.is_none() => reads.record(
                &file.rel,
                &PayloadRead {
                    stamp,
                    read_at,
                    members,
                    header_rule: how.header,
                    disk: None,
                },
            ),
            _ => reads.record_unreadable(&file.rel, &stamp),
        }
    }
    read
}

/// Reads the placed CHD `file`, as `chd` says: in a quick inventory, claims each track by its header's SHA-1; else
/// extracts it and reads each track, and records the read, as any file's. A CD's cue sheet is read from the store.
fn read_chd(file: &ToRead, chd: &ChdRead, how: How, jobs: Hand) -> FileRead {
    // A quick inventory only ever claims a placed CHD's tracks, by its header: never Payload Evidence, not even an
    // earlier read's, so that it never makes its Game Verified.
    if how.evidence == AuditEvidence::Quick {
        let (claimed, stamp) = claim_chd(file, chd, jobs, how.header);
        if let (Some(reads), Some(stamp), None) = (how.reads, stamp, &claimed.error) {
            reads.claimed(&file.rel, &stamp);
        }
        return claimed;
    }
    // Its tracks' earlier read is reused while the CHD's stamp holds; its cue sheet, which the CHD doesn't hold, is
    // checked against what the store keeps on every audit, never reused.
    if let Some(reads) = how.reads {
        let now = file.stamp();
        if let (Some(now), Some(prior)) = (now, reads.prior(&file.rel))
            && prior.stamp == now
            && prior.header_rule == how.header
            && prior.disk.is_none()
        {
            reads.reused(&file.rel, &now);
            let mut members: Vec<_> = prior
                .members
                .into_iter()
                .filter(|(m, _)| !is_cue(m.as_deref()))
                .map(|(m, sums)| (m, Ok(Known::Payload(sums))))
                .collect();
            members.extend(cue_member(&chd.placed, jobs, how.header));
            return FileRead {
                members,
                error: None,
                origin: Origin::Reused(prior.read_at),
            };
        }
    }
    let read_at = jiff::Timestamp::now();
    let (read, stamp) = extract_chd(file, chd, jobs, read_at, how);
    how.tally.file_read();
    if let (Some(reads), Some(stamp)) = (how.reads, stamp) {
        // What is recorded to reuse is its tracks' read alone.
        let members: Option<Vec<_>> = read
            .members
            .iter()
            .filter(|(m, _)| !is_cue(m.as_deref()))
            .map(|(m, sums)| match sums {
                Ok(Known::Payload(sums)) => Some((m.clone(), sums.clone())),
                _ => None,
            })
            .collect();
        match members {
            Some(members) if read.error.is_none() => reads.record(
                &file.rel,
                &PayloadRead {
                    stamp,
                    read_at,
                    members,
                    header_rule: how.header,
                    disk: None,
                },
            ),
            _ => reads.record_unreadable(&file.rel, &stamp),
        }
    }
    read
}

/// Reads the CHD `file` as a Disk (ADR 0026): its header's combined SHA-1, claimed from the header in a quick
/// inventory, and otherwise proven by `chdman verify`, or by an earlier verification while the file's stamp holds. A
/// delta CHD, or a file that is no CHD v5, is read no further.
fn read_disk(file: &ToRead, how: How) -> FileRead {
    let mut read = FileRead {
        members: Vec::new(),
        error: None,
        origin: Origin::Claimed,
    };
    let opened = match file.open() {
        Ok(opened) => opened,
        Err(e) => {
            read.error = Some(e.to_string());
            return read;
        }
    };
    let stamp = opened
        .metadata()
        .ok()
        .and_then(|m| FileStamp::from_metadata(&m));
    let unreadable = |read: &mut FileRead, why: String| {
        if let (Some(reads), Some(stamp)) = (how.reads, stamp) {
            reads.record_unreadable(&file.rel, &stamp);
        }
        read.error = Some(why);
    };
    let header = match crate::chd::read_header(&opened) {
        Ok(header) => header,
        Err(crate::chd::HeaderRefusal::Unread(why)) => {
            unreadable(&mut read, why);
            return read;
        }
        // Recorded as a file it couldn't read, so that a repair can tell whether it changed since.
        Err(refusal) => {
            if let (Some(reads), Some(stamp)) = (how.reads, stamp) {
                reads.record_unreadable(&file.rel, &stamp);
            }
            read.members
                .push((None, Ok(Known::NoDisk(refusal.as_disk()))));
            return read;
        }
    };
    if let Some(parent) = header.parent_sha1 {
        if let (Some(reads), Some(stamp)) = (how.reads, stamp) {
            reads.record_unreadable(&file.rel, &stamp);
        }
        read.members.push((None, Ok(Known::Delta(parent))));
        return read;
    }
    // A quick inventory only ever claims a Disk, by its header: never Payload Evidence, not even an earlier read's.
    if how.evidence == AuditEvidence::Quick {
        if let (Some(reads), Some(stamp)) = (how.reads, stamp) {
            reads.claimed(&file.rel, &stamp);
        }
        read.members.push((None, Ok(Known::Disk(header.sha1))));
        return read;
    }
    if let (Some(reads), Some(now)) = (how.reads, stamp)
        && let Some(prior) = reads.prior(&file.rel)
        && prior.stamp == now
        && let Some(sha1) = prior.disk
    {
        reads.reused(&file.rel, &now);
        read.origin = Origin::Reused(prior.read_at);
        read.members.push((None, Ok(Known::Disk(sha1))));
        return read;
    }
    let read_at = jiff::Timestamp::now();
    read.origin = Origin::Read(read_at);
    let verified = match how.disks {
        Some(disks) => disks.verify(&file.path, &header),
        None => Err("chdman isn't available to this audit, so its CHD can't be verified".into()),
    };
    how.tally.file_read();
    // The file chdman read by its path is still the one held open, as it was before.
    let still = stamp.is_some() && FileStamp::of(&file.path) == stamp && {
        let now = opened.metadata().ok();
        now.and_then(|m| FileStamp::from_metadata(&m)) == stamp
    };
    match verified {
        Ok(()) if still => {
            if let (Some(reads), Some(stamp)) = (how.reads, stamp) {
                reads.record(
                    &file.rel,
                    &PayloadRead {
                        stamp,
                        read_at,
                        members: Vec::new(),
                        header_rule: how.header,
                        disk: Some(header.sha1.clone()),
                    },
                );
            }
            read.members.push((None, Ok(Known::Disk(header.sha1))));
        }
        Ok(()) => unreadable(&mut read, "it changed while chdman verified it".into()),
        Err(why) => unreadable(&mut read, why),
    }
    read
}

/// A placed CHD's disc: whether it is a CD or a DVD, and the ROMs its tracks are, in order. Its cue sheet's read, or
/// why it can't be.
type Disc<'r> = (
    title_select_api::storage::ChdKind,
    Vec<&'r title_select_api::build::ExpectedRom>,
);

fn disc<'r>(chd: &'r crate::PlacedChd) -> Result<Disc<'r>, String> {
    if let Some(Ok(cue)) = &chd.cue {
        return crate::chd::disc_of(&chd.game, &chd.roms, Some(cue));
    }
    // Without its cue sheet, which its own member then says can't be read, its tracks are taken in the DAT's order:
    // each is still matched by its payload, or claimed by its size and CRC32.
    let sized: Vec<(&str, u64)> = chd.roms.iter().map(|r| (r.name.as_str(), r.size)).collect();
    let kind = title_select_api::storage::chd_of(&chd.game, &sized).map_err(str::to_owned)?;
    Ok((
        kind,
        chd.roms.iter().filter(|r| !is_cue(Some(&r.name))).collect(),
    ))
}

/// Whether `member` of a placed CHD's read is its cue sheet, which the store keeps rather than the CHD.
fn is_cue(member: Option<&str>) -> bool {
    member.is_some_and(|m| m.to_ascii_lowercase().ends_with(".cue"))
}

/// The cue sheet member of a placed CHD's read, read from where the store keeps it, if it has one.
fn cue_member(
    chd: &crate::PlacedChd,
    jobs: Hand,
    header: Option<HeaderRule>,
) -> Option<(Option<String>, Result<Known, String>)> {
    let rom = chd.roms.iter().find(|r| is_cue(Some(&r.name)))?;
    let known = match &chd.cue {
        Some(Ok(cue)) => checksum(cue.as_slice(), jobs, header).map(Known::Payload),
        Some(Err(why)) => Err(format!(
            "its cue sheet, kept in the store, can't be read: {why}"
        )),
        None => Err("no cue sheet is kept for it in the store".into()),
    };
    Some((Some(rom.name.clone()), known))
}

/// Claims each track of the placed CHD `file` from its header, while the header's SHA-1 is the one it held when it was
/// placed: a track by the size and CRC32 the DAT gave it then.
fn claim_chd(
    file: &ToRead,
    chd: &ChdRead,
    jobs: Hand,
    header: Option<HeaderRule>,
) -> (FileRead, Option<FileStamp>) {
    let mut read = FileRead {
        members: Vec::new(),
        error: None,
        origin: Origin::Claimed,
    };
    let placed = &chd.placed;
    // Its cue sheet is claimed with its tracks, by the size and CRC32 of the bytes the store keeps: an inventory makes
    // no Game Verified.
    read.members
        .extend(cue_member(placed, jobs, header).map(|(name, known)| {
            let claim = known.map(|known| match known {
                Known::Payload(sums) => Known::Claim {
                    size: sums.size,
                    crc: sums.crc,
                },
                claim => claim,
            });
            (name, claim)
        }));
    let opened = match file.open() {
        Ok(opened) => opened,
        Err(e) => {
            read.error = Some(e.to_string());
            return (read, None);
        }
    };
    let stamp = opened
        .metadata()
        .ok()
        .and_then(|m| FileStamp::from_metadata(&m));
    let claimed = crate::chd::header(&opened).and_then(|h| match h.sha1 == placed.sha1 {
        true => disc(placed),
        false => Err(
            "its CHD header no longer holds the SHA-1 it was placed with, so it claims nothing; a full audit reads \
             what it holds"
                .into(),
        ),
    });
    match claimed {
        Ok((_, bins)) => {
            for rom in bins {
                let known = rom
                    .crc
                    .clone()
                    .map(|crc| Known::Claim {
                        size: rom.size,
                        crc,
                    })
                    .ok_or_else(|| "the DAT gave no CRC32 to claim it by".to_owned());
                read.members.push((Some(rom.name.clone()), known));
            }
        }
        Err(why) => read.error = Some(why),
    }
    (read, stamp)
}

/// One CHD's folder in the scratch directory, removed with what is in it once dropped.
pub(crate) struct ChdScratch(pub(crate) PathBuf);

impl Drop for ChdScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Extracts the placed CHD `file` from a private copy in the scratch directory, and reads each track it gives back as
/// the ROM it is to be; and a CD's cue sheet from the store. Returns what was read, and the file as it was when opened.
fn extract_chd(
    file: &ToRead,
    chd: &ChdRead,
    jobs: Hand,
    read_at: jiff::Timestamp,
    how: How,
) -> (FileRead, Option<FileStamp>) {
    static SCRATCH: AtomicUsize = AtomicUsize::new(0);
    let mut read = FileRead {
        members: Vec::new(),
        error: None,
        origin: Origin::Read(read_at),
    };
    let placed = &chd.placed;
    read.members.extend(cue_member(placed, jobs, how.header));
    let opened = match file.open() {
        Ok(opened) => opened,
        Err(e) => {
            read.error = Some(e.to_string());
            return (read, None);
        }
    };
    let stamp = opened
        .metadata()
        .ok()
        .and_then(|m| FileStamp::from_metadata(&m));
    // A folder of its own, removed with what is in it once every track is read.
    let dir = chd.audit.tmp.join(format!(
        "audit-chd-{}-{}",
        std::process::id(),
        SCRATCH.fetch_add(1, Ordering::Relaxed)
    ));
    let _scratch = ChdScratch(dir.clone());
    let extracted = (|| -> Result<Vec<(String, PathBuf)>, String> {
        let (kind, bins) = disc(placed)?;
        let chdman = crate::chd::Chdman {
            deadline: chd
                .audit
                .limits
                .for_bytes(placed.roms.iter().map(|r| r.size).sum()),
            ..chd.audit.chdman.clone()?
        };
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        crate::chd::copy_for_chdman(&opened, kind, &bins, &dir.join("out.chd"), how.tally)?;
        let given = chdman
            .extract(&dir, kind, &bins, &|| false)
            .map_err(|stop| match stop {
                crate::chd::Stop::Failed(why) | crate::chd::Stop::Corrupt(why) => why,
                crate::chd::Stop::Cancelled => "chdman was stopped".to_owned(),
            })?;
        Ok(bins.iter().map(|r| r.name.clone()).zip(given).collect())
    })();
    match extracted {
        Ok(tracks) => {
            // Read in the scratch directory, not from any Root.
            let scratch = Tally::default();
            for (name, path) in tracks {
                let sums = File::open(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|f| checksum(Reading::new(f, &scratch), jobs, how.header));
                read.members.push((Some(name), sums.map(Known::Payload)));
            }
        }
        Err(why) => read.error = Some(why),
    }
    (read, stamp)
}

/// Opens `file` as a ZIP archive once its central directory is known to be within bounds.
fn bounded_archive<R: Read + Seek>(
    file: &File,
    reader: impl FnOnce() -> std::io::Result<R>,
) -> Result<zip::ZipArchive<R>, String> {
    use crate::zipdir::{DirectoryLimits, Gated, open};
    match open(file, &DirectoryLimits::default(), reader).map_err(|e| e.to_string())? {
        Gated::Archive(archive) => archive.map_err(|e| e.to_string()),
        Gated::Refused(why) => Err(format!("not read: {why}")),
        Gated::NotZip => Err("not read: it is not a ZIP archive".into()),
    }
}

/// Claims each member of the ZIP archive `file` from its directory: its name, size and CRC32.
fn claim_file(file: &ToRead) -> FileRead {
    let mut read = FileRead {
        members: Vec::new(),
        error: None,
        origin: Origin::Claimed,
    };
    let mut zip = match file
        .open()
        .map_err(|e| e.to_string())
        .and_then(|f| bounded_archive(&f, || f.try_clone()))
    {
        Ok(zip) => zip,
        Err(e) => {
            read.error = Some(e);
            return read;
        }
    };
    for i in 0..zip.len() {
        let member = match zip.by_index_raw(i) {
            Ok(member) => member,
            Err(e) => {
                read.error = Some(e.to_string());
                return read;
            }
        };
        if member.is_dir() {
            continue;
        }
        let name = match member.name() {
            Ok(name) => name.into_owned(),
            Err(e) => {
                read.error = Some(e.to_string());
                return read;
            }
        };
        let claim = Known::Claim {
            size: member.size(),
            crc: format!("{:08x}", member.crc32()),
        };
        read.members.push((Some(name), Ok(claim)));
    }
    read
}

/// Reads every payload of `file`, returning what was read and the file as it was when opened.
fn read_file(
    to_read: &ToRead,
    jobs: Hand,
    read_at: jiff::Timestamp,
    how: How,
) -> (FileRead, Option<FileStamp>) {
    let mut read = FileRead {
        members: Vec::new(),
        error: None,
        origin: Origin::Read(read_at),
    };
    let is_zip = to_read.is_zip;
    let file = match to_read.open() {
        Ok(file) => file,
        Err(e) => {
            read.error = Some(e.to_string());
            return (read, None);
        }
    };
    let stamp = file
        .metadata()
        .ok()
        .and_then(|m| FileStamp::from_metadata(&m));
    if !is_zip {
        match checksum(Reading::new(file, how.tally), jobs, how.header) {
            Ok(sums) => read.members.push((None, Ok(Known::Payload(sums)))),
            Err(e) => read.error = Some(e),
        }
        return (read, stamp);
    }
    // Only a member's stored bytes are counted, as they are read: not the directory, nor the headers read to find it.
    let payload = Cell::new(false);
    let opened = || {
        file.try_clone()
            .map(|f| Switched::new(f, how.tally, &payload))
    };
    let mut zip = match bounded_archive(&file, opened) {
        Ok(zip) => zip,
        Err(e) => {
            read.error = Some(e);
            return (read, stamp);
        }
    };
    for i in 0..zip.len() {
        let member = match zip.by_index(i) {
            Ok(member) => member,
            Err(e) => {
                read.error = Some(e.to_string());
                return (read, stamp);
            }
        };
        if member.is_dir() {
            continue;
        }
        let name = match member.name() {
            Ok(name) => name.into_owned(),
            Err(e) => {
                read.error = Some(e.to_string());
                return (read, stamp);
            }
        };
        payload.set(true);
        let sums = checksum(member, jobs, how.header);
        payload.set(false);
        read.members.push((Some(name), sums.map(Known::Payload)));
    }
    (read, stamp)
}

/// Reads `r` to its end, once, handing its bytes to a hash worker, and returns their checksums without the Header
/// `header` finds.
fn checksum(mut r: impl Read, jobs: Hand, header: Option<HeaderRule>) -> Result<Checksums, String> {
    let (chunks, chunks_rx) = sync_channel(CHUNKS_IN_FLIGHT);
    let (done_tx, done) = sync_channel(1);
    // Waits for a free hash worker.
    let handed = jobs(HashJob {
        chunks: chunks_rx,
        header,
        done: done_tx,
    });
    if !handed {
        return Err("no hash worker is running".to_owned());
    }
    loop {
        let mut buf = vec![0u8; CHUNK];
        let n = r.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        buf.truncate(n);
        chunks
            .send(buf)
            .map_err(|_| "the hash worker stopped".to_owned())?;
    }
    drop(chunks);
    done.recv()
        .map_err(|_| "the hash worker stopped".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that counts the bytes it hands out.
    struct Counting<'a> {
        bytes: &'a [u8],
        read: &'a AtomicUsize,
    }

    impl Read for Counting<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.bytes.read(buf)?;
            self.read.fetch_add(n, Ordering::Relaxed);
            Ok(n)
        }
    }

    /// Notes how many files are being read at once as each starts: from `prior`, called as a reader starts a file,
    /// to `record`, called as it ends. The first four files to start wait for one another, and so do the 43rd to the
    /// 45th, so that as many as the pace allows are surely read at once then.
    struct Overlap {
        reading: AtomicUsize,
        starts: Mutex<Vec<usize>>,
        first: std::sync::Barrier,
        later: std::sync::Barrier,
    }

    impl PayloadReads for Overlap {
        fn prior(&self, _: &str) -> Option<PayloadRead> {
            let now = self.reading.fetch_add(1, Ordering::SeqCst) + 1;
            let index = {
                let mut starts = self.starts.lock().unwrap();
                starts.push(now);
                starts.len() - 1
            };
            match index {
                0..4 => drop(self.first.wait()),
                42..45 => drop(self.later.wait()),
                _ => {}
            }
            None
        }

        fn record(&self, _: &str, _: &PayloadRead) {
            self.reading.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn files(dir: &Path, n: usize) -> Vec<ToRead> {
        (0..n)
            .map(|i| {
                let path = dir.join(format!("{i}.bin"));
                std::fs::write(&path, [i as u8; 32]).unwrap();
                ToRead {
                    path,
                    is_zip: false,
                    rel: format!("{i}.bin"),
                    beneath: None,
                    chd: None,
                    disk: false,
                }
            })
            .collect()
    }

    #[test]
    fn a_change_of_pace_changes_how_many_files_are_read_at_once_from_the_next_file() {
        bounded(|| {
            let dir = tempfile::tempdir().unwrap();
            let files = files(dir.path(), 60);
            let overlap = Overlap {
                reading: AtomicUsize::new(0),
                starts: Mutex::default(),
                first: std::sync::Barrier::new(4),
                later: std::sync::Barrier::new(3),
            };
            let how = How {
                evidence: AuditEvidence::Full,
                reads: Some(&overlap),
                header: None,
                might_be_headered: &|_| false,
                tally: &Default::default(),
                disks: None,
            };
            let (readers, hashers) = (Pool::new(4), Pool::new(4));
            // Four at once for the first 12 files, one at a time to the 36th, then three at once.
            let progress = |p: Progress| match p.files_done {
                12 => readers.set_limit(1),
                36 => readers.set_limit(3),
                _ => {}
            };
            let ceilings = Workers {
                readers: 4,
                hashers: 4,
            };
            let pools = Pools {
                readers: &readers,
                hashers: &hashers,
            };

            let read = read_files(&files, how, ceilings, Some(pools), Some(&progress)).unwrap();

            assert!(read.iter().all(|r| r.error.is_none()));
            let starts = overlap.starts.into_inner().unwrap();
            let most = |range: std::ops::Range<usize>| starts[range].iter().copied().max().unwrap();
            assert_eq!(most(0..12), 4, "{starts:?}");
            // The files under way when the pace dropped finish; every file started after them is read alone.
            assert_eq!(most(18..36), 1, "{starts:?}");
            assert_eq!(most(42..60), 3, "{starts:?}");
        });
    }

    /// Counts the files being read at once across every Job that shares it. The first `meeting` files to start once
    /// it is open wait for one another, so that that many are surely read at once.
    #[derive(Default)]
    struct Shared {
        reading: AtomicUsize,
        most: AtomicUsize,
        open: AtomicBool,
        meeting: usize,
        met: AtomicUsize,
        meet: Option<std::sync::Barrier>,
    }

    impl Shared {
        fn meeting(meeting: usize, open: bool) -> Shared {
            Shared {
                open: AtomicBool::new(open),
                meeting,
                meet: Some(std::sync::Barrier::new(meeting)),
                ..Shared::default()
            }
        }
    }

    impl PayloadReads for Shared {
        fn prior(&self, _: &str) -> Option<PayloadRead> {
            let now = self.reading.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(now, Ordering::SeqCst);
            if let Some(meet) = &self.meet
                && self.open.load(Ordering::SeqCst)
                && self.met.fetch_add(1, Ordering::SeqCst) < self.meeting
            {
                meet.wait();
            }
            None
        }

        fn record(&self, _: &str, _: &PayloadRead) {
            self.reading.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Runs `f` on a thread of its own, failing if it takes longer than 30 s.
    fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = sync_channel(1);
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .expect("it finished in time: no deadlock, and no permit stuck")
    }

    #[test]
    fn jobs_reading_one_root_share_its_readers_and_the_hosts_hash_workers() {
        bounded(|| {
            let dir = tempfile::tempdir().unwrap();
            let files = files(dir.path(), 24);
            let shared = Shared::meeting(2, true);
            let (readers, hashers) = (Pool::new(2), Pool::new(1));
            let pools = Pools {
                readers: &readers,
                hashers: &hashers,
            };
            // Each Job may start more threads than the pools allow, as the ceilings it started with.
            let each = Workers {
                readers: 4,
                hashers: 4,
            };
            std::thread::scope(|s| {
                for _ in 0..3 {
                    s.spawn(|| {
                        let how = How {
                            evidence: AuditEvidence::Full,
                            reads: Some(&shared),
                            header: None,
                            might_be_headered: &|_| false,
                            tally: &Default::default(),
                            disks: None,
                        };
                        let read = read_files(&files, how, each, Some(pools), None).unwrap();
                        assert!(read.iter().all(|r| r.error.is_none()));
                    });
                }
            });
            assert_eq!(
                shared.most.load(Ordering::SeqCst),
                2,
                "across all three jobs"
            );
            assert_eq!(readers.most_at_once(), 2);
            assert_eq!(hashers.most_at_once(), 1);
            assert_eq!((readers.taken(), hashers.taken()), (0, 0));
        });
    }

    #[test]
    fn jobs_waiting_on_each_others_pools_never_deadlock() {
        // One reader permit and one hash worker permit for everyone, and many Jobs each holding a reader permit
        // while it waits for a hash worker: they finish, one payload at a time.
        bounded(|| {
            let dir = tempfile::tempdir().unwrap();
            let files = files(dir.path(), 16);
            let (readers, hashers) = (Pool::new(1), Pool::new(1));
            let pools = Pools {
                readers: &readers,
                hashers: &hashers,
            };
            let each = Workers {
                readers: 3,
                hashers: 3,
            };
            std::thread::scope(|s| {
                for _ in 0..4 {
                    s.spawn(|| {
                        let how = How {
                            evidence: AuditEvidence::Full,
                            reads: None,
                            header: None,
                            might_be_headered: &|_| false,
                            tally: &Default::default(),
                            disks: None,
                        };
                        let read = read_files(&files, how, each, Some(pools), None).unwrap();
                        assert!(read.iter().all(|r| r.error.is_none()));
                    });
                }
            });
            assert_eq!((readers.taken(), hashers.taken()), (0, 0));
        });
    }

    /// Files of `size` bytes each, numbered.
    fn files_of(dir: &Path, n: usize, size: usize) -> Vec<ToRead> {
        (0..n)
            .map(|i| {
                let path = dir.join(format!("{i}.bin"));
                let bytes: Vec<u8> = (0..size).map(|b| (b * 31 + i) as u8).collect();
                std::fs::write(&path, bytes).unwrap();
                ToRead {
                    path,
                    is_zip: false,
                    rel: format!("{i}.bin"),
                    beneath: None,
                    chd: None,
                    disk: false,
                }
            })
            .collect()
    }

    #[test]
    fn a_running_job_starts_more_workers_than_it_began_with_when_its_ceilings_rise() {
        bounded(|| {
            let cpus = std::thread::available_parallelism().unwrap().get();
            let dir = tempfile::tempdir().unwrap();
            let files = files_of(dir.path(), 400, 64 << 10);
            // 33 files at once, once the readers rise.
            let shared = Shared::meeting(33, false);
            let (readers, hashers) = (Pool::new(4), Pool::new(1));
            let pools = Pools {
                readers: &readers,
                hashers: &hashers,
            };
            // You raise its readers from 4 to 64, and its hash workers past the CPU count, after its 10th file.
            let progress = |p: Progress| {
                if p.files_done == 10 {
                    readers.set_limit(64);
                    hashers.set_limit(cpus + 8);
                    shared.open.store(true, Ordering::SeqCst);
                }
            };
            let how = How {
                evidence: AuditEvidence::Full,
                reads: Some(&shared),
                header: None,
                might_be_headered: &|_| false,
                tally: &Default::default(),
                disks: None,
            };
            let began = Workers {
                readers: 4,
                hashers: 1,
            };

            let (read, started) =
                read_counting_threads(&files, how, began, Some(pools), Some(&progress), &|_| false)
                    .unwrap();

            assert!(read.iter().all(|r| r.error.is_none()));
            let most = shared.most.load(Ordering::SeqCst);
            assert!(most > 32, "{most} files at once, {started:?}");
            assert!(readers.most_at_once() > 32);
            assert!(
                hashers.most_at_once() > cpus,
                "{} hash workers at once over {cpus} CPUs, {started:?}",
                hashers.most_at_once()
            );
            assert!(
                started.readers <= 64 && started.hashers <= cpus + 8,
                "{started:?}"
            );
        });
    }

    #[test]
    fn a_small_job_starts_no_more_workers_than_it_has_files() {
        bounded(|| {
            let dir = tempfile::tempdir().unwrap();
            let files = files(dir.path(), 3);
            let (readers, hashers) = (Pool::new(64), Pool::new(64));
            let pools = Pools {
                readers: &readers,
                hashers: &hashers,
            };
            let how = How {
                evidence: AuditEvidence::Full,
                reads: Some(&Shared::default()),
                header: None,
                might_be_headered: &|_| false,
                tally: &Default::default(),
                disks: None,
            };
            let each = Workers {
                readers: 64,
                hashers: 64,
            };
            let (read, started) =
                read_counting_threads(&files, how, each, Some(pools), None, &|_| false).unwrap();
            assert!(read.iter().all(|r| r.error.is_none()));
            assert!(started.readers <= 3 && started.hashers <= 3, "{started:?}");
            assert!(started.readers >= 1 && started.hashers >= 1, "{started:?}");
        });
    }

    #[test]
    fn a_job_whose_first_hash_worker_cant_start_fails_rather_than_hanging_its_readers() {
        bounded(|| {
            let dir = tempfile::tempdir().unwrap();
            let files = files_of(dir.path(), 40, 64 << 10);
            let (readers, hashers) = (Pool::new(8), Pool::new(8));
            let pools = Pools {
                readers: &readers,
                hashers: &hashers,
            };
            let how = How {
                evidence: AuditEvidence::Full,
                reads: None,
                header: None,
                might_be_headered: &|_| false,
                tally: &Default::default(),
                disks: None,
            };
            let each = Workers {
                readers: 8,
                hashers: 8,
            };
            let readers_started = AtomicUsize::new(0);
            // The system refuses the hash worker only once every reader the pool allows has started, each to hand over
            // a payload meanwhile.
            let refuse = |kind: Worker| match kind {
                Worker::Reader => {
                    readers_started.fetch_add(1, Ordering::SeqCst);
                    false
                }
                Worker::Hasher => {
                    while readers_started.load(Ordering::SeqCst) < 8 {
                        std::thread::yield_now();
                    }
                    true
                }
            };

            let failed = read_counting_threads(&files, how, each, Some(pools), None, &refuse);

            let why = failed.err().expect("the Job fails");
            assert!(why.contains("hash worker"), "{why}");
            assert_eq!(readers_started.load(Ordering::SeqCst), 8);
            assert_eq!((readers.taken(), hashers.taken()), (0, 0));
        });
    }

    #[test]
    fn a_job_carries_on_with_the_workers_it_has_when_more_cant_start() {
        bounded(|| {
            let dir = tempfile::tempdir().unwrap();
            let files = files_of(dir.path(), 200, 64 << 10);
            let (readers, hashers) = (Pool::new(8), Pool::new(8));
            let pools = Pools {
                readers: &readers,
                hashers: &hashers,
            };
            let how = How {
                evidence: AuditEvidence::Full,
                reads: None,
                header: None,
                might_be_headered: &|_| false,
                tally: &Default::default(),
                disks: None,
            };
            let each = Workers {
                readers: 8,
                hashers: 8,
            };
            let (asked_readers, asked_hashers) = (AtomicUsize::new(0), AtomicUsize::new(0));
            // Two readers and one hash worker start; the system refuses every other.
            let refuse = |kind: Worker| match kind {
                Worker::Reader => asked_readers.fetch_add(1, Ordering::SeqCst) >= 2,
                Worker::Hasher => asked_hashers.fetch_add(1, Ordering::SeqCst) >= 1,
            };

            let (read, started) =
                read_counting_threads(&files, how, each, Some(pools), None, &refuse).unwrap();

            assert!(read.iter().all(|r| r.error.is_none()));
            assert_eq!(
                started,
                Started {
                    readers: 2,
                    hashers: 1
                }
            );
            assert!(asked_readers.load(Ordering::SeqCst) > 2);
            assert!(asked_hashers.load(Ordering::SeqCst) > 1);
            assert_eq!((readers.taken(), hashers.taken()), (0, 0));
        });
    }

    /// Panics as the third file starts.
    struct Panics(AtomicUsize);

    impl PayloadReads for Panics {
        fn prior(&self, _: &str) -> Option<PayloadRead> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 2 {
                panic!("a reader fails mid-job");
            }
            None
        }

        fn record(&self, _: &str, _: &PayloadRead) {}
    }

    #[test]
    fn a_job_that_panics_gives_back_every_permit_to_the_jobs_sharing_its_pools() {
        bounded(|| {
            let dir = tempfile::tempdir().unwrap();
            let files = files(dir.path(), 8);
            let (readers, hashers) = (Pool::new(2), Pool::new(2));
            let pools = Pools {
                readers: &readers,
                hashers: &hashers,
            };
            let each = Workers {
                readers: 2,
                hashers: 2,
            };
            let panics = Panics(AtomicUsize::new(0));
            let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let how = How {
                    evidence: AuditEvidence::Full,
                    reads: Some(&panics),
                    header: None,
                    might_be_headered: &|_| false,
                    tally: &Default::default(),
                    disks: None,
                };
                read_files(&files, how, each, Some(pools), None).unwrap()
            }));
            assert!(failed.is_err());
            assert_eq!((readers.taken(), hashers.taken()), (0, 0));
            // Another Job reads all it has with the same pools.
            let how = How {
                evidence: AuditEvidence::Full,
                reads: None,
                header: None,
                might_be_headered: &|_| false,
                tally: &Default::default(),
                disks: None,
            };
            let read = read_files(&files, how, each, Some(pools), None).unwrap();
            assert!(read.iter().all(|r| r.error.is_none()));
        });
    }

    #[test]
    fn a_headered_payload_is_read_and_hashed_once_without_its_header() {
        let mut header = b"NES\x1a".to_vec();
        header.resize(16, 0);
        let rom: Vec<u8> = (0..(3 * CHUNK as u32)).map(|i| (i * 7) as u8).collect();
        let dump = [header.as_slice(), &rom].concat();
        let read = AtomicUsize::new(0);
        let (jobs, queue) = sync_channel::<HashJob>(0);
        let fed = AtomicUsize::new(0);
        let fed_ref = &fed;
        let sums = std::thread::scope(|s| {
            s.spawn(move || {
                let job = queue.recv().unwrap();
                let mut hasher = Hasher::with_header(job.header);
                for chunk in job.chunks {
                    fed_ref.fetch_add(chunk.len(), Ordering::Relaxed);
                    hasher.update(&chunk);
                }
                job.done.send(hasher.finish()).unwrap();
            });
            let counting = Counting {
                bytes: &dump,
                read: &read,
            };
            checksum(counting, &|j| jobs.send(j).is_ok(), Some(HeaderRule::Nes)).unwrap()
        });

        assert_eq!(read.load(Ordering::Relaxed), dump.len(), "read once");
        assert_eq!(fed.load(Ordering::Relaxed), dump.len(), "hashed once");
        assert_eq!(sums.header, Some(16));
        assert_eq!(sums.size, rom.len() as u64);
        assert_eq!(sums.crc, format!("{:08x}", crc32fast::hash(&rom)));
    }
}
