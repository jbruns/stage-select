//! The throughput meter: the bytes and files each Job reads and writes, counted for the Job, for the Root they were
//! read from or written into, and in total (ADR 0022).
//!
//! Counting is a relaxed atomic add on a buffer already in hand: no copy, syscall or lock is added to a read or a
//! write. A [`Tally`] is resolved once, for a Job and a Root, before any byte moves, and every count goes through
//! [`Tally::read`] or [`Tally::wrote`]: that is where a Root's byte-rate cap (#193) takes its tokens.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use title_select_api::{Rates, Workers};

/// Bytes and files read and written, counted as they move.
#[derive(Debug, Default)]
pub struct Counts {
    read: AtomicU64,
    written: AtomicU64,
    files_read: AtomicU64,
    files_written: AtomicU64,
}

impl Counts {
    fn now(&self) -> Counted {
        Counted {
            read: self.read.load(Ordering::Relaxed),
            written: self.written.load(Ordering::Relaxed),
            files_read: self.files_read.load(Ordering::Relaxed),
            files_written: self.files_written.load(Ordering::Relaxed),
        }
    }
}

/// What [`Counts`] held at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counted {
    pub read: u64,
    pub written: u64,
    pub files_read: u64,
    pub files_written: u64,
}

/// Where one Job's bytes from or into one Root are counted. A detached tally ([`Tally::default`]) counts nothing.
#[derive(Debug, Clone, Default)]
pub struct Tally(Option<Arc<Sinks>>);

#[derive(Debug)]
struct Sinks {
    job: JobMeter,
    root: Option<Arc<Counts>>,
}

impl Sinks {
    fn each(&self, add: impl Fn(&Counts)) {
        add(&self.job.0.counts);
        if let Some(root) = &self.root {
            add(root);
        }
        add(&self.job.0.total);
    }
}

impl Tally {
    /// Counts `n` payload bytes read.
    #[inline]
    pub fn read(&self, n: usize) {
        if let Some(sinks) = &self.0 {
            sinks.each(|c| {
                c.read.fetch_add(n as u64, Ordering::Relaxed);
            });
        }
    }

    /// Counts `n` bytes written.
    #[inline]
    pub fn wrote(&self, n: usize) {
        if let Some(sinks) = &self.0 {
            sinks.each(|c| {
                c.written.fetch_add(n as u64, Ordering::Relaxed);
            });
        }
    }

    /// Counts one file whose payloads were read.
    pub fn file_read(&self) {
        if let Some(sinks) = &self.0 {
            sinks.each(|c| {
                c.files_read.fetch_add(1, Ordering::Relaxed);
            });
        }
    }

    /// Counts one file written.
    pub fn file_written(&self) {
        if let Some(sinks) = &self.0 {
            sinks.each(|c| {
                c.files_written.fetch_add(1, Ordering::Relaxed);
            });
        }
    }

    /// The Job this tally counts for, if any.
    pub fn job(&self) -> Option<&JobMeter> {
        self.0.as_ref().map(|s| &s.job)
    }
}

/// Every Job's counts, every Root's, and the total, since the Service started.
#[derive(Debug, Default)]
pub struct Meter {
    total: Arc<Counts>,
    roots: Arc<RwLock<BTreeMap<String, Arc<Counts>>>>,
    jobs: Mutex<BTreeMap<i64, JobMeter>>,
}

/// One Job's counts, and how many of its payload readers, hash workers and chdman runs are at work now. Cloning it
/// shares them.
#[derive(Debug, Clone)]
pub struct JobMeter(Arc<JobShared>);

#[derive(Debug)]
struct JobShared {
    id: i64,
    started: Instant,
    ceilings: Workers,
    counts: Counts,
    readers: AtomicUsize,
    hashers: AtomicUsize,
    chdman: AtomicUsize,
    total: Arc<Counts>,
    roots: Arc<RwLock<BTreeMap<String, Arc<Counts>>>>,
}

/// What a Job read and wrote, and for how long it was metered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended {
    pub counted: Counted,
    pub duration: Duration,
}

/// One worker at work, counted until dropped.
pub struct Active<'a>(&'a AtomicUsize);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn active(count: &AtomicUsize) -> Active<'_> {
    count.fetch_add(1, Ordering::Relaxed);
    Active(count)
}

impl Meter {
    /// Starts metering Job `id`, whose payload readers and hash workers are `ceilings` before the pace, from now; the
    /// Job already metered if it is.
    pub fn start(&self, id: i64, ceilings: Workers) -> JobMeter {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(id)
            .or_insert_with(|| {
                JobMeter(Arc::new(JobShared {
                    id,
                    started: Instant::now(),
                    ceilings,
                    counts: Counts::default(),
                    readers: AtomicUsize::new(0),
                    hashers: AtomicUsize::new(0),
                    chdman: AtomicUsize::new(0),
                    total: self.total.clone(),
                    roots: self.roots.clone(),
                }))
            })
            .clone()
    }

    /// Job `id`'s meter, started now with no worker ceilings if it wasn't already.
    pub fn job(&self, id: i64) -> JobMeter {
        self.start(
            id,
            Workers {
                readers: 0,
                hashers: 0,
            },
        )
    }

    /// Stops metering Job `id`, returning what it read and wrote; none if it wasn't metered.
    pub fn end(&self, id: i64) -> Option<Ended> {
        let job = self
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id)?;
        Some(Ended {
            counted: job.0.counts.now(),
            duration: job.0.started.elapsed(),
        })
    }

    /// Everything counted so far.
    pub fn total(&self) -> Counted {
        self.total.now()
    }

    /// What was counted for the Root named `root` so far.
    pub fn root(&self, root: &str) -> Counted {
        self.roots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(root)
            .map(|c| c.now())
            .unwrap_or_default()
    }

    /// The counts now, with each metered Job's workers at work.
    pub fn sample(&self) -> Sample {
        let jobs = self
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|j| JobSample {
                id: j.0.id,
                counted: j.0.counts.now(),
                readers: j.0.readers.load(Ordering::Relaxed),
                hashers: j.0.hashers.load(Ordering::Relaxed),
                chdman: j.0.chdman.load(Ordering::Relaxed) > 0,
                ceilings: j.0.ceilings,
            })
            .collect();
        let roots = self
            .roots
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(name, c)| (name.clone(), c.now()))
            .collect();
        Sample {
            total: self.total.now(),
            roots,
            jobs,
        }
    }
}

impl JobMeter {
    /// Where this Job's bytes from or into the Root named `root` are counted; with no Root, only for the Job and in
    /// total.
    pub fn tally(&self, root: Option<&str>) -> Tally {
        let root = root.map(|name| {
            if let Some(counts) = self
                .0
                .roots
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .get(name)
            {
                return counts.clone();
            }
            self.0
                .roots
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(name.to_owned())
                .or_default()
                .clone()
        });
        Tally(Some(Arc::new(Sinks {
            job: self.clone(),
            root,
        })))
    }

    /// What this Job has read and written so far.
    pub fn counted(&self) -> Counted {
        self.0.counts.now()
    }

    /// Counts a payload reader at work until the guard drops.
    pub fn reading(&self) -> Active<'_> {
        active(&self.0.readers)
    }

    /// Counts a hash worker at work until the guard drops.
    pub fn hashing(&self) -> Active<'_> {
        active(&self.0.hashers)
    }

    /// Counts a chdman run, which isn't byte-capped, until the guard drops.
    pub fn running_chdman(&self) -> Active<'_> {
        active(&self.0.chdman)
    }

    /// Makes this the Job the current thread runs, for code too far from the Job to be handed its meter, until the
    /// guard drops.
    pub fn on_this_thread(&self) -> OnThread {
        OnThread(THIS_THREAD.replace(Some(self.clone())))
    }
}

thread_local! {
    static THIS_THREAD: std::cell::RefCell<Option<JobMeter>> = const { std::cell::RefCell::new(None) };
}

/// The meter of the Job the current thread runs, if one is set.
pub fn this_thread() -> Option<JobMeter> {
    THIS_THREAD.with_borrow(Clone::clone)
}

/// The Job set to the current thread until dropped, which restores the one before.
pub struct OnThread(Option<JobMeter>);

impl Drop for OnThread {
    fn drop(&mut self) {
        THIS_THREAD.set(self.0.take());
    }
}

/// The meter at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sample {
    pub total: Counted,
    pub roots: Vec<(String, Counted)>,
    pub jobs: Vec<JobSample>,
}

/// One Job in a [`Sample`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSample {
    pub id: i64,
    pub counted: Counted,
    pub readers: usize,
    pub hashers: usize,
    pub chdman: bool,
    pub ceilings: Workers,
}

/// Rates over a sliding window: the change between the oldest and newest counts it holds, over the time between them.
#[derive(Debug, Clone)]
pub struct Window {
    span: Duration,
    seen: VecDeque<(Instant, Counted)>,
}

impl Window {
    /// A window over the last `span`.
    pub fn new(span: Duration) -> Window {
        Window {
            span,
            seen: VecDeque::new(),
        }
    }

    /// Adds the counts at `at`, forgets those older than the span, and returns the rates over what is left.
    pub fn push(&mut self, at: Instant, counted: Counted) -> Rates {
        self.seen.push_back((at, counted));
        while self
            .seen
            .front()
            .is_some_and(|(then, _)| at.duration_since(*then) > self.span)
            && self.seen.len() > 2
        {
            self.seen.pop_front();
        }
        self.rates()
    }

    /// The rates over the counts held; none moved while fewer than two are held.
    pub fn rates(&self) -> Rates {
        let (Some((t0, c0)), Some((t1, c1))) = (self.seen.front(), self.seen.back()) else {
            return Rates::default();
        };
        let secs = t1.duration_since(*t0).as_secs_f64();
        if secs <= 0.0 {
            return Rates::default();
        }
        let per_s = |a: u64, b: u64| b.saturating_sub(a) as f64 / secs;
        Rates {
            read_bytes_per_s: per_s(c0.read, c1.read),
            write_bytes_per_s: per_s(c0.written, c1.written),
            files_per_s: per_s(
                c0.files_read + c0.files_written,
                c1.files_read + c1.files_written,
            ),
        }
    }
}

/// A writer that counts every byte written through it.
pub struct Metered<'t, W> {
    inner: W,
    tally: &'t Tally,
}

impl<'t, W> Metered<'t, W> {
    pub fn new(inner: W, tally: &'t Tally) -> Self {
        Metered { inner, tally }
    }

    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for Metered<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.tally.wrote(n);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek> Seek for Metered<'_, W> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

/// A reader that counts every payload byte read through it.
pub(crate) struct Reading<'t, R> {
    inner: R,
    tally: &'t Tally,
}

impl<'t, R> Reading<'t, R> {
    pub(crate) fn new(inner: R, tally: &'t Tally) -> Self {
        Reading { inner, tally }
    }
}

impl<R: Read> Read for Reading<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.tally.read(n);
        Ok(n)
    }
}

impl<R: Seek> Seek for Reading<'_, R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

/// The bytes a copy asks the kernel for at once: a multiple of every ZFS record size and of the Btrfs and XFS block
/// sizes, so a filesystem that shares blocks still can, and small enough that each chunk is counted as it goes.
pub const COPY_CHUNK: usize = 64 << 20;

/// Copies `from`, from its offset to its end, into `to` at its offset, counting each chunk as written to `tally`.
/// On Linux each chunk is one `copy_file_range`, which lets the filesystem share or offload it; where the kernel or
/// filesystem refuses it, and elsewhere, the rest is copied byte by byte. Returns the bytes copied.
pub fn copy_chunked(from: &File, to: &File, tally: &Tally) -> io::Result<u64> {
    copy_with(from, to, tally, COPY_CHUNK, cfg!(target_os = "linux"))
}

/// [`copy_chunked`], in chunks of `chunk` bytes, trying `copy_file_range` first only if `in_kernel`.
pub(crate) fn copy_with(
    from: &File,
    to: &File,
    tally: &Tally,
    chunk: usize,
    in_kernel: bool,
) -> io::Result<u64> {
    let mut copied = 0u64;
    if in_kernel {
        match copy_in_kernel(from, to, tally, chunk, &mut copied) {
            Ok(true) => return Ok(copied),
            Ok(false) => {}
            Err(e) => return Err(e),
        }
    }
    // Byte by byte from where the kernel stopped, if it copied any: each chunk advanced both offsets.
    let mut reader = from;
    let mut out = Metered::new(to, tally);
    let mut buf = vec![0; chunk.min(1 << 20)];
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        out.write_all(&buf[..n])?;
        copied += n as u64;
    }
    Ok(copied)
}

/// Copies with `copy_file_range` until the end of `from`, returning true; false, having counted what it copied in
/// `copied`, when the kernel or filesystem can't copy this way and the rest must be copied byte by byte.
#[cfg(target_os = "linux")]
fn copy_in_kernel(
    from: &File,
    to: &File,
    tally: &Tally,
    chunk: usize,
    copied: &mut u64,
) -> io::Result<bool> {
    use rustix::io::Errno;
    loop {
        match rustix::fs::copy_file_range(from, None, to, None, chunk) {
            Ok(0) => return Ok(true),
            Ok(n) => {
                tally.wrote(n);
                *copied += n as u64;
            }
            Err(Errno::INTR) => {}
            Err(Errno::XDEV | Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP) => return Ok(false),
            // Refused outright, as for a file open for appending.
            Err(Errno::PERM | Errno::BADF | Errno::OVERFLOW) if *copied == 0 => return Ok(false),
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn copy_in_kernel(_: &File, _: &File, _: &Tally, _: usize, _: &mut u64) -> io::Result<bool> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workers() -> Workers {
        Workers {
            readers: 2,
            hashers: 2,
        }
    }

    #[test]
    fn a_tally_counts_for_its_job_its_root_and_in_total() {
        let meter = Meter::default();
        let one = meter.start(1, workers());
        let two = meter.start(2, workers());
        one.tally(Some("roms")).read(100);
        one.tally(Some("library")).wrote(40);
        two.tally(Some("roms")).read(7);
        two.tally(None).wrote(3);
        one.tally(Some("roms")).file_read();

        assert_eq!(
            one.counted(),
            Counted {
                read: 100,
                written: 40,
                files_read: 1,
                files_written: 0
            }
        );
        assert_eq!(meter.root("roms").read, 107);
        assert_eq!(meter.root("library").written, 40);
        assert_eq!(
            meter.total(),
            Counted {
                read: 107,
                written: 43,
                files_read: 1,
                files_written: 0
            }
        );
        let ended = meter.end(1).unwrap();
        assert_eq!(ended.counted.read, 100);
        assert!(meter.end(1).is_none(), "ended once");
        assert_eq!(meter.total().read, 107, "the total outlives the job");
    }

    #[test]
    fn a_detached_tally_counts_nothing() {
        let meter = Meter::default();
        Tally::default().read(10);
        assert_eq!(meter.total(), Counted::default());
    }

    #[test]
    fn a_window_rates_the_change_over_its_span() {
        let mut window = Window::new(Duration::from_secs(3));
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let c = |read: u64, files: u64| Counted {
            read,
            written: read / 2,
            files_read: files,
            files_written: 0,
        };
        assert_eq!(window.push(at(0), c(0, 0)), Rates::default());
        let r = window.push(at(1), c(100, 1));
        assert_eq!(r.read_bytes_per_s, 100.0);
        assert_eq!(r.write_bytes_per_s, 50.0);
        assert_eq!(r.files_per_s, 1.0);
        window.push(at(2), c(300, 2));
        // At 6 s the counts from 0 s to 2 s are older than the span; the rate is over 2 s to 6 s, when nothing moved.
        window.push(at(5), c(300, 2));
        let r = window.push(at(6), c(300, 2));
        assert_eq!(r.read_bytes_per_s, 0.0);
    }

    #[test]
    fn active_workers_are_counted_while_their_guards_live() {
        let meter = Meter::default();
        let job = meter.start(1, workers());
        let a = job.reading();
        let _b = job.reading();
        let _h = job.hashing();
        let _c = job.running_chdman();
        let s = meter.sample();
        assert_eq!((s.jobs[0].readers, s.jobs[0].hashers), (2, 1));
        assert!(s.jobs[0].chdman);
        drop(a);
        assert_eq!(meter.sample().jobs[0].readers, 1);
    }

    fn copy_test(in_kernel: bool) {
        let dir = tempfile::tempdir().unwrap();
        let bytes: Vec<u8> = (0..100_003u32).map(|i| (i * 31 % 251) as u8).collect();
        let src = dir.path().join("src");
        std::fs::write(&src, &bytes).unwrap();
        let dst = dir.path().join("dst");
        let meter = Meter::default();
        let tally = meter.start(1, workers()).tally(Some("library"));
        let from = File::open(&src).unwrap();
        let to = File::create_new(&dst).unwrap();

        let copied = copy_with(&from, &to, &tally, 4096, in_kernel).unwrap();

        assert_eq!(copied, bytes.len() as u64);
        assert_eq!(std::fs::read(&dst).unwrap(), bytes, "byte-identical");
        assert_eq!(meter.root("library").written, bytes.len() as u64);
        assert_eq!(meter.total().written, bytes.len() as u64);
    }

    #[test]
    fn a_chunked_copy_is_byte_identical_and_counted() {
        copy_test(true);
    }

    #[test]
    fn the_byte_copy_fallback_is_byte_identical_and_counted() {
        copy_test(false);
    }
}
