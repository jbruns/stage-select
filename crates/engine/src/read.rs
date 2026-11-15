//! Reading payloads with bounded workers: `readers` threads open files and read (and decompress) their payloads,
//! handing each payload's bytes in chunks to one of `hashers` threads that checksums it. A file whose earlier read
//! passes the change test is not read again, and a quick Audit claims a ZIP archive's members from its directory.
//!
//! `workers` gives the threads started, the ceilings. A pace may hold some of them back: each reader and hash worker
//! passes a [`Gate`] before each file or payload, which lets in only as many as the pace allows at that moment.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use crate::payload::{Checksums, Hasher};
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
}

/// One payload for a hash worker: its chunks, the Header Rule to hash them under, and where to send its checksums.
struct HashJob {
    chunks: Receiver<Vec<u8>>,
    header: Option<HeaderRule>,
    done: SyncSender<Checksums>,
}

/// How often a worker held back by a [`Gate`] looks at the pace again, so that a raised pace lets it in.
const RECHECK: Duration = Duration::from_millis(50);

/// Lets in as many workers at once as a limit, read again at each entry, allows. Lowering the limit stops no worker
/// already in; it holds back the next ones until enough have left.
#[derive(Default)]
pub(crate) struct Gate {
    inside: Mutex<usize>,
    left: Condvar,
}

/// A worker's place inside a [`Gate`], given up when dropped.
pub(crate) struct Pass<'g>(&'g Gate);

impl Gate {
    /// Waits until fewer than `limit()`, and at least one, are inside, then enters.
    pub(crate) fn enter(&self, limit: impl Fn() -> usize) -> Pass<'_> {
        let mut inside = self.inside.lock().unwrap_or_else(PoisonError::into_inner);
        while *inside >= limit().max(1) {
            inside = self
                .left
                .wait_timeout(inside, RECHECK)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *inside += 1;
        Pass(self)
    }
}

impl Drop for Pass<'_> {
    fn drop(&mut self) {
        *self.0.inside.lock().unwrap_or_else(PoisonError::into_inner) -= 1;
        self.0.left.notify_one();
    }
}

/// The counts a pace allows now, read at each file and payload.
pub(crate) type Pace<'a> = &'a (dyn Fn() -> Workers + Sync);

/// Reads every file in `files`, returning one [`FileRead`] per file in the same order. `workers` threads are
/// started, and `pace`, when given, says how many of them may work at each moment.
pub fn read_files(
    files: &[ToRead],
    how: How,
    workers: Workers,
    pace: Option<Pace>,
    progress: Option<&(dyn Fn(Progress) + Sync)>,
) -> Vec<FileRead> {
    let readers = workers.readers.clamp(1, files.len().max(1));
    let hashers = workers.hashers.max(1);
    let (reading, hashing) = (Gate::default(), Gate::default());
    let allowed = |of: fn(Workers) -> usize, ceiling: usize| pace.map_or(ceiling, |p| of(p()));
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<FileRead>>> = Mutex::new(files.iter().map(|_| None).collect());
    let (jobs, queue) = sync_channel::<HashJob>(0);
    let queue = Mutex::new(queue);
    std::thread::scope(|s| {
        for _ in 0..hashers {
            s.spawn(|| {
                loop {
                    let _pass = hashing.enter(|| allowed(|w| w.hashers, hashers));
                    let job = queue.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok(job) = job else { break };
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
            let (next, done, results, reading, allowed) =
                (&next, &done, &results, &reading, &allowed);
            s.spawn(move || {
                loop {
                    let _pass = reading.enter(|| allowed(|w| w.readers, readers));
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(file) = files.get(i) else {
                        break;
                    };
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
    let (read, stamp) = read_file(&file.path, file.is_zip, jobs, read_at, how.header);
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
fn bounded_archive(file: File) -> Result<zip::ZipArchive<File>, String> {
    use crate::zipdir::{DirectoryLimits, Gated, open};
    match open(&file, &DirectoryLimits::default(), || file.try_clone())
        .map_err(|e| e.to_string())?
    {
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
        .and_then(|f| bounded_archive(f).map_err(|e| e.to_string()))
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
    header: Option<HeaderRule>,
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
        match checksum(file, jobs, header) {
            Ok(sums) => read.members.push((None, Ok(Known::Payload(sums)))),
            Err(e) => read.error = Some(e),
        }
        return (read, stamp);
    }
    let mut zip = match bounded_archive(file) {
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
        read.members.push((
            Some(name),
            checksum(member, jobs, header).map(Known::Payload),
        ));
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

    #[test]
    fn a_change_of_pace_changes_how_many_files_are_read_at_once_from_the_next_file() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<ToRead> = (0..60)
            .map(|i| {
                let path = dir.path().join(format!("{i}.bin"));
                std::fs::write(&path, [i as u8; 32]).unwrap();
                ToRead {
                    path,
                    is_zip: false,
                    rel: format!("{i}.bin"),
                }
            })
            .collect();
        let overlap = Overlap {
            reading: AtomicUsize::new(0),
            starts: Mutex::default(),
        };
        let how = How {
            evidence: AuditEvidence::Full,
            reads: Some(&overlap),
            header: None,
            might_be_headered: &|_| false,
        };
        let limit = AtomicUsize::new(4);
        let pace = || Workers {
            readers: limit.load(Ordering::SeqCst),
            hashers: limit.load(Ordering::SeqCst),
        };
        // Four at once for the first 12 files, one at a time to the 36th, then three at once.
        let progress = |p: Progress| match p.files_done {
            12 => limit.store(1, Ordering::SeqCst),
            36 => limit.store(3, Ordering::SeqCst),
            _ => {}
        };
        let ceilings = Workers {
            readers: 4,
            hashers: 4,
        };

        let read = read_files(&files, how, ceilings, Some(&pace), Some(&progress));

        assert!(read.iter().all(|r| r.error.is_none()));
        let starts = overlap.starts.into_inner().unwrap();
        let most = |range: std::ops::Range<usize>| starts[range].iter().copied().max().unwrap();
        assert_eq!(most(0..12), 4, "{starts:?}");
        // The files under way when the pace dropped finish; every file started after them is read alone.
        assert_eq!(most(18..36), 1, "{starts:?}");
        assert_eq!(most(42..60), 3, "{starts:?}");
    }

    #[test]
    fn a_gate_lets_in_as_many_as_its_limit_says_now() {
        let gate = Gate::default();
        let limit = AtomicUsize::new(3);
        let (inside, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let passes = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for _ in 0..6 {
                s.spawn(|| {
                    loop {
                        let _pass = gate.enter(|| limit.load(Ordering::SeqCst));
                        let n = passes.fetch_add(1, Ordering::SeqCst);
                        if n >= 120 {
                            break;
                        }
                        if n == 40 {
                            limit.store(1, Ordering::SeqCst);
                            most.store(0, Ordering::SeqCst);
                        }
                        let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                        if n >= 50 {
                            most.fetch_max(now, Ordering::SeqCst);
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        inside.fetch_sub(1, Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(most.load(Ordering::SeqCst), 1);
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
