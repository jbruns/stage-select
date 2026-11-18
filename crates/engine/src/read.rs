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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
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
/// and hash workers may work at each moment; without them, `workers` does.
pub fn read_files(
    files: &[ToRead],
    how: How,
    workers: Workers,
    pools: Option<Pools>,
    progress: Option<&(dyn Fn(Progress) + Sync)>,
) -> Vec<FileRead> {
    read_counting_threads(files, how, workers, pools, progress).0
}

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
/// workers start their next file, a Job never has more readers than files, and the threads it started stay, waiting
/// for a permit, when the pools shrink. Each start costs a few atomic loads; a file costs nothing more.
struct Crew<'a> {
    files: &'a [ToRead],
    how: How<'a>,
    pools: Pools<'a>,
    progress: Option<&'a (dyn Fn(Progress) + Sync)>,
    next: AtomicUsize,
    done: AtomicUsize,
    results: Mutex<Vec<Option<FileRead>>>,
    queue: Mutex<Receiver<HashJob>>,
    readers: AtomicUsize,
    /// Readers started that hold no permit: waiting for one, or about to.
    readers_waiting: AtomicUsize,
    hashers: AtomicUsize,
    /// Hash workers started that hold no payload.
    hashers_free: AtomicUsize,
}

impl<'a> Crew<'a> {
    fn has_room(pool: &Pool) -> bool {
        pool.taken() < pool.limit()
    }

    /// Starts a reader, counted as waiting until it takes a permit.
    fn start_reader<'s, 'e>(
        &'s self,
        s: &'s std::thread::Scope<'s, 'e>,
        jobs: SyncSender<HashJob>,
    ) {
        self.readers.fetch_add(1, Ordering::SeqCst);
        s.spawn(move || self.read(s, jobs));
    }

    fn read<'s, 'e>(&'s self, s: &'s std::thread::Scope<'s, 'e>, jobs: SyncSender<HashJob>) {
        let files = self.files;
        let all_read = || self.next.load(Ordering::SeqCst) >= files.len();
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
            if i + 1 < files.len()
                && self.readers.load(Ordering::SeqCst) < files.len()
                && Crew::has_room(self.pools.readers)
                && self
                    .readers_waiting
                    .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                self.start_reader(s, jobs.clone());
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

    /// Starts a hash worker, counted as free until it takes a payload, unless one is free already.
    fn start_hasher_unless_one_is_free<'s, 'e>(&'s self, s: &'s std::thread::Scope<'s, 'e>) {
        if self
            .hashers_free
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        self.hashers.fetch_add(1, Ordering::SeqCst);
        s.spawn(move || {
            loop {
                let job = self
                    .queue
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv();
                self.hashers_free.fetch_sub(1, Ordering::SeqCst);
                // The hash workers stop once every reader has dropped its sender.
                let Ok(job) = job else { break };
                {
                    // Taken only with a payload in hand: see the module's note on deadlock.
                    let _permit = self.pools.hashers.enter();
                    let _active = self.how.tally.job().map(JobMeter::hashing);
                    let mut hasher = Hasher::with_header(job.header);
                    for chunk in job.chunks {
                        hasher.update(&chunk);
                    }
                    let _ = job.done.send(hasher.finish());
                }
                self.hashers_free.fetch_add(1, Ordering::SeqCst);
            }
        });
    }
}

fn read_counting_threads(
    files: &[ToRead],
    how: How,
    workers: Workers,
    pools: Option<Pools>,
    progress: Option<&(dyn Fn(Progress) + Sync)>,
) -> (Vec<FileRead>, Started) {
    if files.is_empty() {
        let none = Started {
            readers: 0,
            hashers: 0,
        };
        return (Vec::new(), none);
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
        queue: Mutex::new(queue),
        readers: AtomicUsize::new(0),
        readers_waiting: AtomicUsize::new(1),
        hashers: AtomicUsize::new(0),
        hashers_free: AtomicUsize::new(0),
    };
    std::thread::scope(|s| crew.start_reader(s, jobs));
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
    (results, started)
}

/// Reuses the file's earlier read if it passes the change test; else claims a ZIP archive's members in a quick
/// Audit; else reads its payloads, recording the read when every one of them was read.
fn read_or_reuse(file: &ToRead, how: How, jobs: Hand) -> FileRead {
    if let Some(reads) = how.reads {
        let now = std::fs::metadata(&file.path)
            .ok()
            .and_then(|m| FileStamp::from_metadata(&m));
        if let (Some(now), Some(prior)) = (now, reads.prior(&file.rel))
            && prior.stamp == now
            && prior.header_rule == how.header
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
        let stamp = FileStamp::of(&file.path);
        let claimed = claim_file(&file.path);
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
    let (read, stamp) = read_file(&file.path, file.is_zip, jobs, read_at, how);
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
                },
            ),
            _ => reads.record_unreadable(&file.rel, &stamp),
        }
    }
    read
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

/// Claims each member of the ZIP archive at `path` from its directory: its name, size and CRC32.
fn claim_file(path: &Path) -> FileRead {
    let mut read = FileRead {
        members: Vec::new(),
        error: None,
        origin: Origin::Claimed,
    };
    let mut zip = match File::open(path)
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

/// Reads every payload of the file at `path`, returning what was read and the file as it was when opened.
fn read_file(
    path: &Path,
    is_zip: bool,
    jobs: Hand,
    read_at: jiff::Timestamp,
    how: How,
) -> (FileRead, Option<FileStamp>) {
    let mut read = FileRead {
        members: Vec::new(),
        error: None,
        origin: Origin::Read(read_at),
    };
    let file = match File::open(path) {
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
    /// to `record`, called as it ends.
    struct Overlap {
        reading: AtomicUsize,
        starts: Mutex<Vec<usize>>,
    }

    impl PayloadReads for Overlap {
        fn prior(&self, _: &str) -> Option<PayloadRead> {
            let now = self.reading.fetch_add(1, Ordering::SeqCst) + 1;
            self.starts.lock().unwrap().push(now);
            std::thread::sleep(std::time::Duration::from_millis(15));
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
                }
            })
            .collect()
    }

    #[test]
    fn a_change_of_pace_changes_how_many_files_are_read_at_once_from_the_next_file() {
        let dir = tempfile::tempdir().unwrap();
        let files = files(dir.path(), 60);
        let overlap = Overlap {
            reading: AtomicUsize::new(0),
            starts: Mutex::default(),
        };
        let how = How {
            evidence: AuditEvidence::Full,
            reads: Some(&overlap),
            header: None,
            might_be_headered: &|_| false,
            tally: &Default::default(),
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

        let read = read_files(&files, how, ceilings, Some(pools), Some(&progress));

        assert!(read.iter().all(|r| r.error.is_none()));
        let starts = overlap.starts.into_inner().unwrap();
        let most = |range: std::ops::Range<usize>| starts[range].iter().copied().max().unwrap();
        assert_eq!(most(0..12), 4, "{starts:?}");
        // The files under way when the pace dropped finish; every file started after them is read alone.
        assert_eq!(most(18..36), 1, "{starts:?}");
        assert_eq!(most(42..60), 3, "{starts:?}");
    }

    /// Counts the files being read at once across every Job that shares it.
    #[derive(Default)]
    struct Shared {
        reading: AtomicUsize,
        most: AtomicUsize,
    }

    impl PayloadReads for Shared {
        fn prior(&self, _: &str) -> Option<PayloadRead> {
            let now = self.reading.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(3));
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
            let shared = Shared::default();
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
                        };
                        let read = read_files(&files, how, each, Some(pools), None);
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
                        };
                        let read = read_files(&files, how, each, Some(pools), None);
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
            let shared = Shared::default();
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
                }
            };
            let how = How {
                evidence: AuditEvidence::Full,
                reads: Some(&shared),
                header: None,
                might_be_headered: &|_| false,
                tally: &Default::default(),
            };
            let began = Workers {
                readers: 4,
                hashers: 1,
            };

            let (read, started) =
                read_counting_threads(&files, how, began, Some(pools), Some(&progress));

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
            };
            let each = Workers {
                readers: 64,
                hashers: 64,
            };
            let (read, started) = read_counting_threads(&files, how, each, Some(pools), None);
            assert!(read.iter().all(|r| r.error.is_none()));
            assert!(started.readers <= 3 && started.hashers <= 3, "{started:?}");
            assert!(started.readers >= 1 && started.hashers >= 1, "{started:?}");
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
                };
                read_files(&files, how, each, Some(pools), None)
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
            };
            let read = read_files(&files, how, each, Some(pools), None);
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
