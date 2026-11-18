//! Reading payloads with bounded workers: `readers` threads open files and read (and decompress) their payloads,
//! handing each payload's bytes in chunks to one of `hashers` threads that checksums it. A file whose earlier read
//! passes the change test is not read again, and a quick Audit claims a ZIP archive's members from its directory.
//!
//! `workers` gives the threads started. [`Pools`] may hold some of them back: each reader takes a permit of its
//! Root's readers pool before each file, and each hash worker one of the Host's hash workers pool before each payload
//! (ADR 0022). Other Jobs share the pools, whose size is the pace's share of each Ceiling at that moment.
//!
//! A reader holds its permit while it waits for a hash worker; a hash worker takes its permit only once it has a
//! payload, and then waits only for that payload's reader, which already holds its own. So no worker holding a hash
//! worker permit ever waits for a reader permit, and Jobs sharing both pools can't deadlock.

use std::cell::Cell;
use std::fs::File;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Mutex, PoisonError};

use crate::meter::{JobMeter, Reading, Switched, Tally};
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

/// The pools a Job's workers share with every other Job: the payload readers of the Root it reads, and the Host's
/// hash workers.
#[derive(Clone, Copy)]
pub struct Pools<'a> {
    pub readers: &'a Pool,
    pub hashers: &'a Pool,
}

/// Reads every file in `files`, returning one [`FileRead`] per file in the same order. `workers` threads are
/// started, and `pools`, when given, say how many of them may work at each moment; all of them when not.
pub fn read_files(
    files: &[ToRead],
    how: How,
    workers: Workers,
    pools: Option<Pools>,
    progress: Option<&(dyn Fn(Progress) + Sync)>,
) -> Vec<FileRead> {
    let readers = workers.readers.clamp(1, files.len().max(1));
    let hashers = workers.hashers.max(1);
    let (own_readers, own_hashers) = (Pool::new(readers), Pool::new(hashers));
    let pools = pools.unwrap_or(Pools {
        readers: &own_readers,
        hashers: &own_hashers,
    });
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<FileRead>>> = Mutex::new(files.iter().map(|_| None).collect());
    let (jobs, queue) = sync_channel::<HashJob>(0);
    let queue = Mutex::new(queue);
    std::thread::scope(|s| {
        for _ in 0..hashers {
            s.spawn(|| {
                loop {
                    let job = queue.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok(job) = job else { break };
                    // Taken only with a payload in hand: see the module's note on deadlock.
                    let _permit = pools.hashers.enter();
                    let _active = how.tally.job().map(JobMeter::hashing);
                    let mut hasher = Hasher::with_header(job.header);
                    for chunk in job.chunks {
                        hasher.update(&chunk);
                    }
                    let _ = job.done.send(hasher.finish());
                }
            });
        }
        for _ in 0..readers {
            let jobs = jobs.clone();
            let (next, done, results) = (&next, &done, &results);
            s.spawn(move || {
                loop {
                    // A reader with no file left to read doesn't queue for a permit another Job could use.
                    if next.load(Ordering::Relaxed) >= files.len() {
                        break;
                    }
                    let _permit = pools.readers.enter();
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = files.get(i) else {
                        break;
                    };
                    let _active = how.tally.job().map(JobMeter::reading);
                    let read = read_or_reuse(file, how, &jobs);
                    results.lock().unwrap_or_else(PoisonError::into_inner)[i] = Some(read);
                    let files_done = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if let Some(progress) = progress {
                        progress(Progress {
                            files_done,
                            files_total: files.len(),
                        });
                    }
                }
            });
        }
        // The hash workers stop once every reader has dropped its sender.
        drop(jobs);
    });
    results
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .into_iter()
        .map(|r| r.expect("every file is read"))
        .collect()
}

/// Reuses the file's earlier read if it passes the change test; else claims a ZIP archive's members in a quick
/// Audit; else reads its payloads, recording the read when every one of them was read.
fn read_or_reuse(file: &ToRead, how: How, jobs: &SyncSender<HashJob>) -> FileRead {
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
    jobs: &SyncSender<HashJob>,
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
fn checksum(
    mut r: impl Read,
    jobs: &SyncSender<HashJob>,
    header: Option<HeaderRule>,
) -> Result<Checksums, String> {
    let (chunks, chunks_rx) = sync_channel(CHUNKS_IN_FLIGHT);
    let (done_tx, done) = sync_channel(1);
    // Waits for a free hash worker.
    jobs.send(HashJob {
        chunks: chunks_rx,
        header,
        done: done_tx,
    })
    .map_err(|_| "no hash worker is running".to_owned())?;
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
            checksum(counting, &jobs, Some(HeaderRule::Nes)).unwrap()
        });

        assert_eq!(read.load(Ordering::Relaxed), dump.len(), "read once");
        assert_eq!(fed.load(Ordering::Relaxed), dump.len(), "hashed once");
        assert_eq!(sums.header, Some(16));
        assert_eq!(sums.size, rom.len() as u64);
        assert_eq!(sums.crc, format!("{:08x}", crc32fast::hash(&rom)));
    }
}
