//! The throughput meter: the bytes and files each Job reads and writes, counted for the Job, for the Root they were
//! read from or written into, and in total (ADR 0022).
//!
//! Counting is a relaxed atomic add on a buffer already in hand: no copy, syscall or lock is added to a read or a
//! write. A [`Tally`] is resolved once, for a Job and a Root, before any byte moves, and every count goes through
//! [`Tally::read`] or [`Tally::wrote`]: that is where a Root's byte-rate cap (#193) takes its tokens.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use title_select_api::{Rates, Workers};

/// What one thread counted for one [`Flow`], on a cache line of its own. Only that thread writes it, so each count is
/// a plain load and store, never a read-modify-write, and no other thread's counting touches its line; the sampler
/// only loads it.
#[derive(Debug, Default)]
#[repr(align(128))]
struct Slot {
    read: AtomicU64,
    written: AtomicU64,
    files_read: AtomicU64,
    files_written: AtomicU64,
}

impl Slot {
    /// Adds `n` to `count`, which only this slot's thread writes.
    #[inline]
    fn add(count: &AtomicU64, n: u64) {
        count.store(
            count.load(Ordering::Relaxed).wrapping_add(n),
            Ordering::Relaxed,
        );
    }

    fn now(&self) -> Counted {
        Counted {
            read: self.read.load(Ordering::Relaxed),
            written: self.written.load(Ordering::Relaxed),
            files_read: self.files_read.load(Ordering::Relaxed),
            files_written: self.files_written.load(Ordering::Relaxed),
        }
    }
}

/// What one Job counted from or into one Root, or into none: a slot for each thread that counted into it, summed only
/// when sampled.
#[derive(Debug, Default)]
struct Flow {
    slots: Mutex<Vec<Arc<Slot>>>,
}

impl Flow {
    fn now(&self) -> Counted {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .fold(Counted::default(), |sum, slot| sum + slot.now())
    }
}

thread_local! {
    /// This thread's slot in each Flow it has counted into. A Flow is held weakly, so its memory, and the address
    /// matched on, isn't reused while its entry is here; entries of Flows gone are dropped when a new one is added.
    static SLOTS: RefCell<Vec<(Weak<Flow>, Arc<Slot>)>> = const { RefCell::new(Vec::new()) };
}

/// Counts into this thread's slot of `flow`, registering one the first time this thread counts into it.
#[inline]
fn count(flow: &Arc<Flow>, add: impl FnOnce(&Slot)) {
    SLOTS.with_borrow_mut(|slots| {
        if let Some((_, slot)) = slots
            .iter()
            .find(|(f, _)| std::ptr::eq(f.as_ptr(), Arc::as_ptr(flow)))
        {
            return add(slot);
        }
        slots.retain(|(f, _)| f.strong_count() > 0);
        let slot = Arc::new(Slot::default());
        flow.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(slot.clone());
        add(&slot);
        slots.push((Arc::downgrade(flow), slot));
    });
}

/// What was counted, at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counted {
    pub read: u64,
    pub written: u64,
    pub files_read: u64,
    pub files_written: u64,
}

impl std::ops::Add for Counted {
    type Output = Counted;

    fn add(self, o: Counted) -> Counted {
        Counted {
            read: self.read + o.read,
            written: self.written + o.written,
            files_read: self.files_read + o.files_read,
            files_written: self.files_written + o.files_written,
        }
    }
}

/// Where one Job's bytes from or into one Root are counted. A detached tally ([`Tally::default`]) counts nothing.
/// Cloning it, and sharing it between threads, is cheap: each thread counts into a slot of its own.
#[derive(Debug, Clone, Default)]
pub struct Tally(Option<Arc<Sinks>>);

#[derive(Debug)]
struct Sinks {
    job: JobMeter,
    flow: Arc<Flow>,
}

impl Tally {
    /// Counts `n` payload bytes read.
    // A Root's byte-rate cap (#193) takes its tokens here and in `wrote`, before counting, and only under a pace that
    // caps: unthrottled, nothing waits.
    #[inline]
    pub fn read(&self, n: usize) {
        if let Some(sinks) = &self.0 {
            count(&sinks.flow, |s| Slot::add(&s.read, n as u64));
        }
    }

    /// Counts `n` bytes written.
    #[inline]
    pub fn wrote(&self, n: usize) {
        if let Some(sinks) = &self.0 {
            count(&sinks.flow, |s| Slot::add(&s.written, n as u64));
        }
    }

    /// Counts one file whose payloads were read.
    pub fn file_read(&self) {
        if let Some(sinks) = &self.0 {
            count(&sinks.flow, |s| Slot::add(&s.files_read, 1));
        }
    }

    /// Counts one file written.
    pub fn file_written(&self) {
        if let Some(sinks) = &self.0 {
            count(&sinks.flow, |s| Slot::add(&s.files_written, 1));
        }
    }

    /// A tally for Root `root` of the Job the current thread runs, or one that counts nothing when it runs none.
    pub fn of_this_thread(root: Option<&str>) -> Tally {
        this_thread().map_or_else(Tally::default, |job| job.tally(root))
    }

    /// The Job this tally counts for, if any.
    pub fn job(&self) -> Option<&JobMeter> {
        self.0.as_ref().map(|s| &s.job)
    }
}

/// Every Job's counts, every Root's, and the total, since the Service started: summed from the Jobs metered now, and
/// from what the Jobs that ended left behind.
#[derive(Debug, Default)]
pub struct Meter {
    jobs: Mutex<BTreeMap<i64, JobMeter>>,
    ended: Mutex<Left>,
}

/// What the Jobs that ended counted, in total and for each Root.
#[derive(Debug, Default)]
struct Left {
    total: Counted,
    roots: BTreeMap<String, Counted>,
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
    /// By the Root counted from or into; none for what is counted for the Job alone.
    flows: Mutex<BTreeMap<Option<String>, Arc<Flow>>>,
    readers: AtomicUsize,
    hashers: AtomicUsize,
    chdman: AtomicUsize,
}

impl JobShared {
    fn by_root(&self) -> Vec<(Option<String>, Counted)> {
        self.flows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(root, flow)| (root.clone(), flow.now()))
            .collect()
    }
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
                    flows: Mutex::default(),
                    readers: AtomicUsize::new(0),
                    hashers: AtomicUsize::new(0),
                    chdman: AtomicUsize::new(0),
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

    /// Stops metering Job `id`, returning what it read and wrote; none if it wasn't metered. What it counted stays in
    /// its Roots' counts and the total; anything still counted for it after this is not.
    pub fn end(&self, id: i64) -> Option<Ended> {
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let job = jobs.remove(&id)?;
        let by_root = job.0.by_root();
        let mut ended = self.ended.lock().unwrap_or_else(PoisonError::into_inner);
        let mut counted = Counted::default();
        for (root, c) in by_root {
            counted = counted + c;
            if let Some(root) = root {
                let left = ended.roots.entry(root).or_default();
                *left = *left + c;
            }
        }
        ended.total = ended.total + counted;
        Some(Ended {
            counted,
            duration: job.0.started.elapsed(),
        })
    }

    /// Everything counted so far.
    pub fn total(&self) -> Counted {
        self.sample().total
    }

    /// What was counted for the Root named `root` so far.
    pub fn root(&self, root: &str) -> Counted {
        self.sample()
            .roots
            .into_iter()
            .find(|(name, _)| name == root)
            .map(|(_, c)| c)
            .unwrap_or_default()
    }

    /// The counts now, with each metered Job's workers at work.
    pub fn sample(&self) -> Sample {
        let jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let ended = self.ended.lock().unwrap_or_else(PoisonError::into_inner);
        let mut total = ended.total;
        let mut roots = ended.roots.clone();
        let jobs = jobs
            .values()
            .map(|j| {
                let mut counted = Counted::default();
                for (root, c) in j.0.by_root() {
                    counted = counted + c;
                    if let Some(root) = root {
                        let sum = roots.entry(root).or_default();
                        *sum = *sum + c;
                    }
                }
                total = total + counted;
                JobSample {
                    id: j.0.id,
                    counted,
                    readers: j.0.readers.load(Ordering::Relaxed),
                    hashers: j.0.hashers.load(Ordering::Relaxed),
                    chdman: j.0.chdman.load(Ordering::Relaxed) > 0,
                    ceilings: j.0.ceilings,
                }
            })
            .collect();
        Sample {
            total,
            roots: roots.into_iter().collect(),
            jobs,
        }
    }
}

impl JobMeter {
    /// Where this Job's bytes from or into the Root named `root` are counted; with no Root, only for the Job and in
    /// total.
    pub fn tally(&self, root: Option<&str>) -> Tally {
        let flow = self
            .0
            .flows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(root.map(str::to_owned))
            .or_default()
            .clone();
        Tally(Some(Arc::new(Sinks {
            job: self.clone(),
            flow,
        })))
    }

    /// What this Job has read and written so far.
    pub fn counted(&self) -> Counted {
        self.0
            .by_root()
            .into_iter()
            .fold(Counted::default(), |sum, (_, c)| sum + c)
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
    static THIS_THREAD: RefCell<Option<JobMeter>> = const { RefCell::new(None) };
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

/// A reader that counts every byte read through it: wrapped round a file, the bytes read from that file.
pub struct Reading<'t, R> {
    inner: R,
    tally: &'t Tally,
}

impl<'t, R> Reading<'t, R> {
    pub fn new(inner: R, tally: &'t Tally) -> Self {
        Reading { inner, tally }
    }

    pub fn into_inner(self) -> R {
        self.inner
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

/// A reader of an archive file that counts the bytes read through it only while `on` is set: those of a member's
/// stored payload, and not of the directory or the headers read to find it.
pub(crate) struct Switched<'t, R> {
    inner: R,
    tally: &'t Tally,
    on: &'t Cell<bool>,
}

impl<'t, R> Switched<'t, R> {
    pub(crate) fn new(inner: R, tally: &'t Tally, on: &'t Cell<bool>) -> Self {
        Switched { inner, tally, on }
    }
}

impl<R: Read> Read for Switched<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if self.on.get() {
            self.tally.read(n);
        }
        Ok(n)
    }
}

impl<R: Seek> Seek for Switched<'_, R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

/// The bytes a copy asks the kernel for at once: a multiple of every ZFS record size and of the Btrfs and XFS block
/// sizes, so a filesystem that shares blocks still can, and small enough that each chunk is counted as it goes.
pub const COPY_CHUNK: usize = 64 << 20;

/// Copies `from`, from its offset to its end, into `to` at its offset, counting each chunk as read from `from`'s Root
/// to `src` and as written into `to`'s to `dst`, each on its own even where both are the same Root. On Linux each
/// chunk is first one `copy_file_range`, which lets the filesystem share or offload it; where the kernel or filesystem
/// refuses it, and elsewhere, the rest is copied by [`io::copy`] a chunk at a time, which still copies in the kernel
/// where it can. Returns the bytes copied.
pub fn copy_chunked(from: &File, src: &Tally, to: &File, dst: &Tally) -> io::Result<u64> {
    copy_with((from, src), (to, dst), COPY_CHUNK, &mut copy_range)
}

/// What one `copy_file_range` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InKernel {
    /// It copied this many bytes, advancing both offsets; none at the end of the file.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Copied(usize),
    /// The kernel or filesystem can't copy these files this way.
    Refused,
}

/// [`copy_chunked`], in chunks of `chunk` bytes, through `kernel` for each `copy_file_range`.
pub(crate) fn copy_with(
    (from, src): (&File, &Tally),
    (to, dst): (&File, &Tally),
    chunk: usize,
    kernel: &mut dyn FnMut(&File, &File, usize) -> io::Result<InKernel>,
) -> io::Result<u64> {
    let mut copied = 0u64;
    let counted = |n: u64| {
        src.read(n as usize);
        dst.wrote(n as usize);
    };
    loop {
        match kernel(from, to, chunk)? {
            // A first call that copies nothing may be a filesystem that copies nothing this way, as overlayfs once
            // did, rather than an empty file: the copy below tells them apart. Later, nothing is the end.
            InKernel::Copied(0) if copied == 0 => break,
            InKernel::Copied(0) => return Ok(copied),
            InKernel::Copied(n) => {
                counted(n as u64);
                copied += n as u64;
            }
            InKernel::Refused => break,
        }
    }
    // From where the kernel stopped, if it copied any: each chunk advanced both offsets. `io::copy` between two files
    // keeps std's own kernel copies, `sendfile` among them, before it falls back to a buffer.
    loop {
        let reader = from;
        let mut writer = to;
        let n = io::copy(&mut reader.take(chunk as u64), &mut writer)?;
        if n == 0 {
            return Ok(copied);
        }
        counted(n);
        copied += n;
    }
}

/// One `copy_file_range` of up to `chunk` bytes, from and to each file's offset.
#[cfg(target_os = "linux")]
fn copy_range(from: &File, to: &File, chunk: usize) -> io::Result<InKernel> {
    use rustix::io::Errno;
    loop {
        match rustix::fs::copy_file_range(from, None, to, None, chunk) {
            Ok(n) => return Ok(InKernel::Copied(n)),
            Err(Errno::INTR) => {}
            Err(
                Errno::XDEV
                | Errno::INVAL
                | Errno::NOSYS
                | Errno::OPNOTSUPP
                | Errno::PERM
                | Errno::BADF
                | Errno::OVERFLOW,
            ) => return Ok(InKernel::Refused),
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn copy_range(_: &File, _: &File, _: usize) -> io::Result<InKernel> {
    Ok(InKernel::Refused)
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

    #[test]
    fn threads_count_into_slots_of_their_own_that_sum_to_every_byte() {
        let meter = Meter::default();
        let job = meter.start(1, workers());
        let roms = job.tally(Some("roms"));
        let library = job.tally(Some("library"));
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    for _ in 0..1000 {
                        roms.read(3);
                        library.wrote(2);
                    }
                    roms.file_read();
                });
            }
        });
        // Each thread counted in one slot of each Flow it touched.
        assert_eq!(roms.0.as_ref().unwrap().flow.slots.lock().unwrap().len(), 8);
        assert_eq!(std::mem::align_of::<Slot>(), 128);
        assert_eq!(
            job.counted(),
            Counted {
                read: 24_000,
                written: 16_000,
                files_read: 8,
                files_written: 0
            }
        );
        assert_eq!(meter.root("roms").read, 24_000);
        assert_eq!(meter.root("library").written, 16_000);
        assert_eq!(
            job.tally(Some("roms"))
                .0
                .unwrap()
                .flow
                .slots
                .lock()
                .unwrap()
                .len(),
            8,
            "one Flow per Root"
        );
        meter.end(1);
        assert_eq!(meter.root("roms").read, 24_000, "kept once the job ends");
        assert_eq!(meter.total().written, 16_000);
    }

    /// A source file of `len` bytes, and an empty destination, in `dir`.
    fn files(dir: &std::path::Path, len: u32) -> (Vec<u8>, File, File) {
        let bytes: Vec<u8> = (0..len).map(|i| (i * 31 % 251) as u8).collect();
        let src = dir.join("src");
        std::fs::write(&src, &bytes).unwrap();
        let from = File::open(&src).unwrap();
        let to = File::create_new(dir.join("dst")).unwrap();
        (bytes, from, to)
    }

    /// Copies as `copy_file_range` would with no offsets given: up to `chunk` bytes from and to each file's offset.
    fn in_kernel(from: &File, to: &File, chunk: usize) -> io::Result<InKernel> {
        let (from, mut to) = (from, to);
        io::copy(&mut from.take(chunk as u64), &mut to).map(|n| InKernel::Copied(n as usize))
    }

    /// Copies with `kernel`, checking the copy is byte-identical and counted as read from one Root and written into
    /// another; returns how often `kernel` was called.
    fn copy_test(
        len: u32,
        mut kernel: impl FnMut(usize, &File, &File, usize) -> io::Result<InKernel>,
    ) -> usize {
        let dir = tempfile::tempdir().unwrap();
        let (bytes, from, to) = files(dir.path(), len);
        let meter = Meter::default();
        let job = meter.start(1, workers());
        let (src, dst) = (job.tally(Some("roms")), job.tally(Some("library")));
        let mut calls = 0;
        let copied = copy_with((&from, &src), (&to, &dst), 4096, &mut |f, t, c| {
            calls += 1;
            kernel(calls, f, t, c)
        })
        .unwrap();

        assert_eq!(copied, bytes.len() as u64);
        assert_eq!(
            std::fs::read(dir.path().join("dst")).unwrap(),
            bytes,
            "byte-identical"
        );
        assert_eq!(meter.root("roms").read, bytes.len() as u64);
        assert_eq!(meter.root("roms").written, 0);
        assert_eq!(meter.root("library").written, bytes.len() as u64);
        assert_eq!(meter.total().read, bytes.len() as u64);
        assert_eq!(meter.total().written, bytes.len() as u64);
        calls
    }

    #[test]
    fn a_chunked_copy_is_byte_identical_and_counts_each_chunk_read_and_written() {
        let calls = copy_test(100_003, |_, f, t, c| in_kernel(f, t, c));
        assert_eq!(calls, 100_003 / 4096 + 2, "a chunk at a time, then the end");
    }

    #[test]
    fn a_copy_the_kernel_refuses_is_copied_in_full_by_the_fallback() {
        assert_eq!(copy_test(100_003, |_, _, _, _| Ok(InKernel::Refused)), 1);
    }

    #[test]
    fn a_first_copy_of_nothing_falls_back_rather_than_ending_the_copy() {
        assert_eq!(copy_test(100_003, |_, _, _, _| Ok(InKernel::Copied(0))), 1);
    }

    #[test]
    fn a_copy_refused_part_way_is_finished_from_where_the_kernel_stopped() {
        let calls = copy_test(100_003, |call, f, t, c| match call {
            1..=3 => in_kernel(f, t, c),
            _ => Ok(InKernel::Refused),
        });
        assert_eq!(calls, 4);
    }

    #[test]
    fn a_later_copy_of_nothing_is_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let (_, from, to) = files(dir.path(), 10_000);
        let meter = Meter::default();
        let job = meter.start(1, workers());
        let (src, dst) = (job.tally(Some("roms")), job.tally(Some("library")));
        let mut calls = 0;
        let copied = copy_with((&from, &src), (&to, &dst), 4096, &mut |f, t, c| {
            calls += 1;
            match calls {
                1 => in_kernel(f, t, c),
                _ => Ok(InKernel::Copied(0)),
            }
        })
        .unwrap();
        assert_eq!(copied, 4096, "no fallback once the kernel has copied");
        assert_eq!(meter.root("library").written, 4096);
    }

    #[test]
    fn a_copy_into_its_own_root_counts_its_read_and_its_write_each() {
        let dir = tempfile::tempdir().unwrap();
        let (bytes, from, to) = files(dir.path(), 5000);
        let meter = Meter::default();
        let job = meter.start(1, workers());
        let roms = job.tally(Some("roms"));
        copy_chunked(&from, &roms, &to, &roms).unwrap();
        let n = bytes.len() as u64;
        assert_eq!(
            (meter.root("roms").read, meter.root("roms").written),
            (n, n)
        );
    }

    #[test]
    fn a_switched_reader_counts_only_while_on() {
        let meter = Meter::default();
        let tally = meter.start(1, workers()).tally(Some("roms"));
        let on = Cell::new(false);
        let mut r = Switched::new(&[0u8; 100][..], &tally, &on);
        let mut buf = [0; 30];
        r.read_exact(&mut buf).unwrap();
        on.set(true);
        r.read_exact(&mut buf).unwrap();
        on.set(false);
        r.read_exact(&mut buf).unwrap();
        assert_eq!(meter.root("roms").read, 30);
    }
}
