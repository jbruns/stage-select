//! The throughput meter: the bytes and files each Job reads and writes, counted for the Job, for the Root they were
//! read from or written into, and in total (ADR 0022).
//!
//! Counting is a relaxed atomic add on a buffer already in hand: no copy, syscall or lock is added to a read or a
//! write. A [`Tally`] is resolved once, for a Job and a Root, before any byte moves, and every count goes through
//! a Tally: that is where a Root's byte-rate caps ([`crate::cap`]) take their tokens. Under `unthrottled`, where a
//! Root's buckets have no rate, that costs one relaxed load more and touches no bucket.
//!
//! Capped, a read or write is paid for before it is made ([`Tally::read_from`], [`Tally::write_to`] and the readers
//! and writers built on them), a piece of at most a quarter second at the cap at a time, reserved in turn behind
//! every earlier one ([`crate::cap`]): so no thread has bytes in flight the cap hasn't let through. What a short read
//! didn't move is kept by the thread for its next read until its slot passes, never given back into a queue others
//! reserved behind. A copy's chunk is reserved in its source's read cap and its destination's write cap, and moves
//! once both turns have come ([`crate::cap::take_both`]).
//!
//! A cancel stops only the capped I/O under [`Interruptible`], work the Job drops whole when cancelled: it fails with
//! [`Cancelled`] and gives back what it didn't wait for. Everything else a cancelled Job still does, settling or
//! rolling back what it began, waits for its caps as before: a cancel never lets bytes past a cap, nor leaves the
//! next Jobs a debt to pay.
//!
//! A Tally also holds its Root's shared readers pool (ADR 0022), when the Service shares one: every Job-time read of a
//! file of the Root takes one permit of it for that file, through [`Tally::reader`], the first read through a
//! [`Reading`] or [`Switched`], or [`copy_chunked`], and gives it back when that drops. A thread holds at most one
//! reader permit: one asked for while it holds another shares it, and it is given back only once every reader on the
//! thread is done with it; one asked for while the thread holds the store's writer is taken without waiting. So the waits go one way only, 7z gate, then reader, then hash worker, and nothing holding a later one
//! waits for an earlier one: Jobs sharing the pools can't deadlock.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use title_select_api::{Rates, Workers};

use crate::cap::{Bucket, Caps, Clock, Monotonic, Receipt, copy_chunk, piece};
use crate::pool::{OwnedPermit, Pool};

/// The limits every Job shares on each Root, as the Service sets them.
pub trait RootLimits: Send + Sync + std::fmt::Debug {
    /// The readers pool every Job reading the Root `root` shares.
    fn readers(&self, root: &str) -> Arc<Pool>;

    /// The byte-rate caps every Job reading or writing the Root `root` shares; none where it has none at all.
    fn caps(&self, _root: &str) -> Option<Arc<Caps>> {
        None
    }
}

thread_local! {
    /// The reader permit this thread holds, if any, and how many [`ReaderPermit`]s share it.
    static HELD: RefCell<Option<Held>> = const { RefCell::new(None) };
    /// How many [`NoReaderWaits`] this thread is under.
    static NO_WAITS: Cell<usize> = const { Cell::new(0) };
}

/// The one reader permit a thread holds: none where its reader took one itself from a pool it holds.
struct Held {
    _permit: Option<OwnedPermit>,
    guards: usize,
}

/// A share of the one permit of a Root's shared readers pool this thread holds while it reads files of it. Every
/// [`ReaderPermit`] on a thread shares that permit, which is given back only once the last of them drops: a reader
/// of one file may outlive the reader that took the permit, as the sources of a repack do, each finished in turn.
/// One taken where there is no pool, or where the thread may not wait for one, shares nothing.
#[derive(Debug)]
#[must_use = "a reader permit is given back as soon as it drops"]
pub struct ReaderPermit {
    shares: bool,
    /// [`HELD`] is this thread's: the permit stays on it.
    _here: std::marker::PhantomData<*const ()>,
}

impl ReaderPermit {
    const NONE: ReaderPermit = ReaderPermit {
        shares: false,
        _here: std::marker::PhantomData,
    };

    const SHARE: ReaderPermit = ReaderPermit {
        shares: true,
        _here: std::marker::PhantomData,
    };

    /// A share of the permit this thread holds, if it holds one.
    fn share() -> Option<ReaderPermit> {
        HELD.with_borrow_mut(|held| {
            let held = held.as_mut()?;
            held.guards += 1;
            Some(ReaderPermit::SHARE)
        })
    }

    fn hold(permit: Option<OwnedPermit>) -> ReaderPermit {
        HELD.set(Some(Held {
            _permit: permit,
            guards: 1,
        }));
        ReaderPermit::SHARE
    }

    /// Marks this thread as holding a reader permit it took itself, from a pool it holds, until dropped.
    pub(crate) fn held() -> ReaderPermit {
        ReaderPermit::share().unwrap_or_else(|| ReaderPermit::hold(None))
    }

    /// Whether this thread holds a reader permit now.
    pub fn on_this_thread() -> bool {
        HELD.with_borrow(Option::is_some)
    }
}

impl Drop for ReaderPermit {
    fn drop(&mut self) {
        if !self.shares {
            return;
        }
        // Taken out of the cell before it drops, so that giving the permit back wakes no one under the borrow.
        let last = HELD.try_with(|held| {
            let mut held = held.borrow_mut();
            let h = held.as_mut()?;
            h.guards -= 1;
            if h.guards == 0 { held.take() } else { None }
        });
        drop(last);
    }
}

/// While held, this thread waits for no reader permit: the store holds it with its writer, which a Job holding a
/// reader permit may be waiting for.
#[must_use]
pub struct NoReaderWaits(());

impl NoReaderWaits {
    pub fn new() -> NoReaderWaits {
        NO_WAITS.set(NO_WAITS.get() + 1);
        NoReaderWaits(())
    }
}

impl Default for NoReaderWaits {
    fn default() -> Self {
        NoReaderWaits::new()
    }
}

impl Drop for NoReaderWaits {
    fn drop(&mut self) {
        NO_WAITS.set(NO_WAITS.get() - 1);
    }
}

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

impl std::ops::Sub for Counted {
    type Output = Counted;

    /// What was counted since `o`, an earlier reading of the same counts.
    fn sub(self, o: Counted) -> Counted {
        Counted {
            read: self.read - o.read,
            written: self.written - o.written,
            files_read: self.files_read - o.files_read,
            files_written: self.files_written - o.files_written,
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
    /// The Root's shared readers pool, when the Service shares one.
    readers: Option<Arc<Pool>>,
    /// The Root's byte-rate caps, when the Service shares them and the Job's reads and writes are capped.
    caps: Option<Arc<Caps>>,
}

/// Which of a Root's caps a byte counts against.
#[derive(Clone, Copy)]
enum Way {
    Read,
    Write,
}

impl Sinks {
    /// The bucket bytes going `way` are charged to, while it caps them.
    #[inline]
    fn capped(&self, way: Way) -> Option<&Bucket> {
        let caps = self.caps.as_deref()?;
        let bucket = match way {
            Way::Read => &caps.read,
            Way::Write => &caps.write,
        };
        bucket.capped().then_some(bucket)
    }

    /// The clock the Root's caps wait by.
    fn clock(&self) -> &dyn Clock {
        match &self.caps {
            Some(caps) => caps.clock(),
            None => &Monotonic,
        }
    }

    /// Whether this thread's I/O for the Job stops for its cancel: only under [`Interruptible`], and not while the
    /// thread holds the store's writer.
    fn interruptible(&self) -> bool {
        INTERRUPTIBLE.get() > 0 && NO_WAITS.get() == 0
    }

    /// Whether a cancel stops this thread's I/O for the Job now.
    fn stopped(&self) -> bool {
        self.interruptible() && self.job.cancelled()
    }

    /// Reserves `n` bytes in `bucket` before they move, and waits for their turn: not while this thread holds the
    /// store's writer, which other Jobs may be waiting for. The receipt, to give back what doesn't move; none for no
    /// bytes, which reserve and wait for nothing. Under [`Interruptible`], once the Job is cancelled, the bytes may not
    /// move, [`Cancelled`], and the slot is given back if it can be.
    #[cold]
    fn pay(&self, bucket: &Bucket, n: u64) -> io::Result<Option<Receipt>> {
        if n == 0 {
            return Ok(None);
        }
        if self.stopped() {
            return Err(cancelled());
        }
        let clock = self.clock();
        if NO_WAITS.get() > 0 {
            return Ok(bucket.reserve(n, clock.now()));
        }
        bucket
            .take(n, clock, &|| self.stopped())
            .ok_or_else(cancelled)
    }

    /// [`Sinks::pay`], for `n` bytes going through both `a` and `b` at once: a copy's chunk.
    #[cold]
    fn pay_both(&self, a: &Bucket, b: &Bucket, n: u64) -> io::Result<[Option<Receipt>; 2]> {
        if n == 0 {
            return Ok([None, None]);
        }
        if self.stopped() {
            return Err(cancelled());
        }
        let clock = self.clock();
        if NO_WAITS.get() > 0 {
            let now = clock.now();
            return Ok([a.reserve(n, now), b.reserve(n, now)]);
        }
        crate::cap::take_both(a, b, n, clock, &|| self.stopped()).ok_or_else(cancelled)
    }

    /// Reserves `n` bytes that have already moved through `bucket`, and waits for their turn, as [`Sinks::pay`] does;
    /// under [`Interruptible`] once the Job is cancelled, they are not reserved.
    #[cold]
    fn pay_after(&self, bucket: &Bucket, n: u64) {
        let _ = self.pay(bucket, n);
    }

    /// Moves at most a piece of `want` bytes going `way` through `bucket` by `io`, paid for before it moves. What
    /// `io` didn't move this thread keeps, for its next I/O through `bucket` before the slot ends: the read that finds
    /// the end of a file then costs nothing, and leaves what was paid for to the next file's first read. Nothing is
    /// given back to the bucket, so nothing late can give back a slot others queued behind; what a thread keeps and
    /// doesn't use before its slot ends, at most one piece, is capacity lost.
    #[inline(never)]
    fn capped_io(
        &self,
        bucket: &Bucket,
        way: Way,
        want: usize,
        io: impl FnOnce(usize) -> io::Result<usize>,
    ) -> io::Result<usize> {
        if want == 0 {
            return io(0);
        }
        let now = self.clock().now();
        let at = std::ptr::from_ref(bucket) as usize;
        let kept = KEPT
            .with(|k| k[way as usize].take())
            .filter(|k| k.bucket == at && k.left > 0 && k.receipt.open(bucket, now));
        if let Some(mut kept) = kept {
            if self.stopped() {
                return Err(cancelled());
            }
            let len = want.min(usize::try_from(kept.left).unwrap_or(usize::MAX));
            let moved = io(len);
            kept.left -= moved.as_ref().map_or(0, |n| *n as u64);
            KEPT.with(|k| k[way as usize].set(Some(kept)));
            return moved;
        }
        let len = want.min(bucket.rate().map_or(want, piece));
        let receipt = self.pay(bucket, len as u64)?;
        // A change of rate while it waited may let fewer through.
        let len = receipt.map_or(len, |r| {
            len.min(usize::try_from(r.bytes()).unwrap_or(usize::MAX))
        });
        let moved = io(len);
        let n = moved.as_ref().map_or(0, |n| *n);
        if let Some(receipt) = receipt
            && n < len
        {
            let kept = Kept {
                bucket: at,
                receipt,
                left: (len - n) as u64,
            };
            KEPT.with(|k| k[way as usize].set(Some(kept)));
        }
        moved
    }
}

/// Bytes of a slot a thread paid for and didn't move, kept for its next I/O through the same bucket while the slot
/// lasts.
#[derive(Clone, Copy)]
struct Kept {
    /// The bucket's address, to know it again.
    bucket: usize,
    receipt: Receipt,
    left: u64,
}

thread_local! {
    /// What this thread kept of its last capped read, and of its last capped write.
    static KEPT: [Cell<Option<Kept>>; 2] = const { [Cell::new(None), Cell::new(None)] };
}

thread_local! {
    /// How many [`Interruptible`]s this thread is under, and whether a cancel stopped I/O under them.
    static INTERRUPTIBLE: Cell<usize> = const { Cell::new(0) };
    static INTERRUPTED: Cell<bool> = const { Cell::new(false) };
}

/// While held, a cancel of the Job stops this thread's capped reads, writes and copies for it: each fails with
/// [`Cancelled`] rather than wait for a cap, within a recheck of the cap (at most 100 ms). Held only around work the
/// Job can drop halfway: a Build's staging of a file it then removes and its reading of an original it then leaves
/// as it was, and a repair's Build for an item nothing was yet changed for. Everything else, as what settles or rolls
/// back work begun, goes on waiting for its caps after a cancel: a cancel never lets anything past a cap, nor leaves a
/// debt.
#[must_use]
pub struct Interruptible(std::marker::PhantomData<*const ()>);

impl Interruptible {
    pub fn new() -> Interruptible {
        if INTERRUPTIBLE.get() == 0 {
            INTERRUPTED.set(false);
        }
        INTERRUPTIBLE.set(INTERRUPTIBLE.get() + 1);
        Interruptible(std::marker::PhantomData)
    }

    /// Whether a cancel stopped I/O under it.
    pub fn interrupted(&self) -> bool {
        INTERRUPTED.get()
    }
}

impl Default for Interruptible {
    fn default() -> Self {
        Interruptible::new()
    }
}

impl Drop for Interruptible {
    fn drop(&mut self) {
        INTERRUPTIBLE.set(INTERRUPTIBLE.get() - 1);
    }
}

/// Why a read, write or copy under [`Interruptible`] did not happen: you cancelled its Job.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("you cancelled the job")
    }
}

impl std::error::Error for Cancelled {}

fn cancelled() -> io::Error {
    INTERRUPTED.set(true);
    io::Error::other(Cancelled)
}

/// Whether `e` is a read, write or copy stopped by a cancel ([`Cancelled`]).
pub fn is_cancelled(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|e| e.is::<Cancelled>())
}

impl Tally {
    /// Counts `n` payload bytes read, once the Root's read cap, if any, lets them through. A read that can be paid for
    /// before it moves goes through [`Tally::read_from`] instead.
    #[inline]
    pub fn read(&self, n: usize) {
        if let Some(sinks) = &self.0 {
            if let Some(bucket) = sinks.capped(Way::Read) {
                sinks.pay_after(bucket, n as u64);
            }
            count(&sinks.flow, |s| Slot::add(&s.read, n as u64));
        }
    }

    /// Counts `n` bytes written, once the Root's write cap, if any, lets them through. A write that can be paid for
    /// before it moves goes through [`Tally::write_to`] instead.
    #[inline]
    pub fn wrote(&self, n: usize) {
        if let Some(sinks) = &self.0 {
            if let Some(bucket) = sinks.capped(Way::Write) {
                sinks.pay_after(bucket, n as u64);
            }
            count(&sinks.flow, |s| Slot::add(&s.written, n as u64));
        }
    }

    /// Reads from `inner` into `buf`, counting the payload bytes read. Under the Root's read cap, it reads at most a
    /// piece of the cap at once ([`piece`]), once the cap lets it through. Uncapped, it costs a read and one relaxed
    /// load more.
    #[inline]
    pub fn read_from<R: Read + ?Sized>(&self, inner: &mut R, buf: &mut [u8]) -> io::Result<usize> {
        let Some(sinks) = &self.0 else {
            return inner.read(buf);
        };
        let n = match sinks.capped(Way::Read) {
            None => inner.read(buf)?,
            Some(bucket) => sinks.capped_io(bucket, Way::Read, buf.len(), |len| {
                inner.read(&mut buf[..len])
            })?,
        };
        count(&sinks.flow, |s| Slot::add(&s.read, n as u64));
        Ok(n)
    }

    /// Writes `buf` to `inner`, counting the bytes written, as [`Tally::read_from`] reads.
    #[inline]
    pub fn write_to<W: Write + ?Sized>(&self, inner: &mut W, buf: &[u8]) -> io::Result<usize> {
        let Some(sinks) = &self.0 else {
            return inner.write(buf);
        };
        let n = match sinks.capped(Way::Write) {
            None => inner.write(buf)?,
            Some(bucket) => sinks.capped_io(bucket, Way::Write, buf.len(), |len| {
                inner.write(&buf[..len])
            })?,
        };
        count(&sinks.flow, |s| Slot::add(&s.written, n as u64));
        Ok(n)
    }

    /// Counts `n` bytes another process read, such as chdman, which no cap holds back.
    pub fn read_outside(&self, n: usize) {
        self.counted(Way::Read, n as u64);
    }

    /// Counts `n` bytes another process wrote, such as chdman, which no cap holds back.
    pub fn wrote_outside(&self, n: usize) {
        self.counted(Way::Write, n as u64);
    }

    /// Counts `n` bytes going `way`, without charging them.
    fn counted(&self, way: Way, n: u64) {
        if let Some(sinks) = &self.0 {
            count(&sinks.flow, |s| match way {
                Way::Read => Slot::add(&s.read, n),
                Way::Write => Slot::add(&s.written, n),
            });
        }
    }

    /// This tally, with no cap: for a Calibration, which measures what a Root gives uncapped.
    pub fn uncapped(self) -> Tally {
        Tally(self.0.map(|sinks| {
            Arc::new(Sinks {
                job: sinks.job.clone(),
                flow: sinks.flow.clone(),
                readers: sinks.readers.clone(),
                caps: None,
            })
        }))
    }

    /// The bucket bytes going `way` are charged to, while it caps them.
    fn bucket(&self, way: Way) -> Option<&Bucket> {
        self.0.as_ref()?.capped(way)
    }

    /// This Root's bucket for bytes going `way`, capped or not: none for no Root, or for an uncapped tally.
    fn cap(&self, way: Way) -> Option<&Bucket> {
        let caps = self.0.as_ref()?.caps.as_deref()?;
        Some(match way {
            Way::Read => &caps.read,
            Way::Write => &caps.write,
        })
    }

    /// The rate of this Root's cap on bytes going `way`, while it caps them.
    fn rate(&self, way: Way) -> Option<u64> {
        self.bucket(way)?.rate()
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

    /// Waits for a permit of this Root's shared readers pool, to read one file of it, and takes it. Shares the one
    /// this thread holds already, if any, rather than wait for a second, which a ceiling of 1 would never give. Holds
    /// nothing where there is no pool, and under [`NoReaderWaits`].
    pub fn reader(&self) -> ReaderPermit {
        let Some(pool) = self.0.as_ref().and_then(|s| s.readers.as_ref()) else {
            return ReaderPermit::NONE;
        };
        if let Some(share) = ReaderPermit::share() {
            return share;
        }
        if NO_WAITS.get() > 0 {
            return ReaderPermit::NONE;
        }
        ReaderPermit::hold(Some(Pool::enter_owned(pool)))
    }

    /// The readers pool this tally's reads share, if any.
    pub fn readers(&self) -> Option<&Arc<Pool>> {
        self.0.as_ref().and_then(|s| s.readers.as_ref())
    }
}

/// Every Job's counts, every Root's, and the total, since the Service started: summed from the Jobs metered now, and
/// from what the Jobs that ended left behind.
#[derive(Debug, Default)]
pub struct Meter {
    jobs: Mutex<BTreeMap<i64, JobMeter>>,
    ended: Mutex<Left>,
    root_limits: std::sync::OnceLock<Arc<dyn RootLimits>>,
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
    /// Set once the Job is asked to stop: its reads, writes and copies under [`Interruptible`] stop then.
    cancelled: AtomicBool,
    /// The pools its readers and hash workers share with other Jobs, once it reads through them.
    pools: std::sync::OnceLock<(Arc<Pool>, Arc<Pool>)>,
    /// Each Root's shared readers pool and caps, which its tallies take their permits and tokens from.
    root_limits: Option<Arc<dyn RootLimits>>,
}

impl JobShared {
    /// The readers and hash workers it may have at work now: as many as the pools it shares allow, since it starts
    /// threads as they do.
    fn allowance(&self) -> Option<Workers> {
        self.pools.get().map(|(readers, hashers)| Workers {
            readers: readers.limit(),
            hashers: hashers.limit(),
        })
    }

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
    /// Has every Job metered from now take a permit of `limits`' readers pool for each file it reads of a Root, and
    /// the bytes it reads and writes from the Root's caps. Only the first call counts.
    pub fn share_limits(&self, limits: Arc<dyn RootLimits>) {
        let _ = self.root_limits.set(limits);
    }

    /// Stops Job `id`'s capped reads, writes and copies under [`Interruptible`], now and from now on, once it is
    /// asked to stop: each waiting on a cap fails with [`Cancelled`] within a recheck of the cap, and gives back what
    /// it didn't wait for. Its other reads and writes, which settle what it began, go on within its caps.
    pub fn cancel(&self, id: i64) {
        if let Some(job) = self
            .jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
        {
            job.0.cancelled.store(true, Ordering::Relaxed);
        }
    }

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
                    cancelled: AtomicBool::new(false),
                    pools: std::sync::OnceLock::new(),
                    root_limits: self.root_limits.get().cloned(),
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
                let mut its = Vec::new();
                for (root, c) in j.0.by_root() {
                    counted = counted + c;
                    if let Some(root) = root {
                        let sum = roots.entry(root.clone()).or_default();
                        *sum = *sum + c;
                        its.push((root, c));
                    }
                }
                total = total + counted;
                JobSample {
                    id: j.0.id,
                    counted,
                    roots: its,
                    readers: j.0.readers.load(Ordering::Relaxed),
                    hashers: j.0.hashers.load(Ordering::Relaxed),
                    chdman: j.0.chdman.load(Ordering::Relaxed) > 0,
                    ceilings: j.0.ceilings,
                    allowance: j.0.allowance(),
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
    /// Says which pools this Job's readers and hash workers share with other Jobs, so that its allowance follows
    /// them. Only the first call counts.
    pub fn shares(&self, readers: Arc<Pool>, hashers: Arc<Pool>) {
        let _ = self.0.pools.set((readers, hashers));
    }

    /// Where this Job's bytes from or into the Root named `root` are counted; with no Root, only for the Job and in
    /// total.
    pub fn tally(&self, root: Option<&str>) -> Tally {
        let limits = root.zip(self.0.root_limits.as_ref());
        let readers = limits.map(|(root, l)| l.readers(root));
        let caps = limits.and_then(|(root, l)| l.caps(root));
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
            readers,
            caps,
        })))
    }

    /// What this Job has read and written so far.
    pub fn counted(&self) -> Counted {
        self.0
            .by_root()
            .into_iter()
            .fold(Counted::default(), |sum, (_, c)| sum + c)
    }

    /// Whether the Job was asked to stop.
    pub fn cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Relaxed)
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
    /// What it counted in each Root, by name.
    pub roots: Vec<(String, Counted)>,
    pub readers: usize,
    pub hashers: usize,
    pub chdman: bool,
    pub ceilings: Workers,
    /// What the pools it shares allow it now, once it reads through them.
    pub allowance: Option<Workers>,
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

/// A writer that counts every byte written through it, each write paid for before it is made ([`Tally::write_to`]).
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
        self.tally.write_to(&mut self.inner, buf)
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

/// A reader that counts every byte read through it, each read paid for before it is made ([`Tally::read_from`]): wrapped round a file, the bytes read from that file. Its first
/// read takes a permit of the Root's shared readers, held until it drops.
pub struct Reading<'t, R> {
    inner: R,
    tally: &'t Tally,
    permit: Option<ReaderPermit>,
}

impl<'t, R> Reading<'t, R> {
    pub fn new(inner: R, tally: &'t Tally) -> Self {
        Reading {
            inner,
            tally,
            permit: None,
        }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for Reading<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.permit.is_none() {
            self.permit = Some(self.tally.reader());
        }
        self.tally.read_from(&mut self.inner, buf)
    }
}

impl<R: Seek> Seek for Reading<'_, R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

/// A reader of an archive file that counts the bytes read through it only while `on` is set: those of a member's
/// stored payload, and not of the directory or the headers read to find it. Its first read takes a permit of the
/// Root's shared readers, as [`Reading`]'s does.
pub(crate) struct Switched<'t, R> {
    inner: R,
    tally: &'t Tally,
    on: &'t Cell<bool>,
    permit: Option<ReaderPermit>,
}

impl<'t, R> Switched<'t, R> {
    pub(crate) fn new(inner: R, tally: &'t Tally, on: &'t Cell<bool>) -> Self {
        Switched {
            inner,
            tally,
            on,
            permit: None,
        }
    }
}

impl<R: Read> Read for Switched<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.permit.is_none() {
            self.permit = Some(self.tally.reader());
        }
        match self.on.get() {
            true => self.tally.read_from(&mut self.inner, buf),
            false => self.inner.read(buf),
        }
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
///
/// Under a cap, each chunk is charged before it moves, as read from `from`'s Root and as written into `to`'s, even
/// where the filesystem clones it: a clone can't be told from a copy. A capped chunk is smaller where the cap is
/// low ([`copy_chunk`]) and starts at a multiple of its size, so a filesystem that clones blocks still can; it is
/// charged no more than what is left of the file, so that a small file waits for its own bytes only. A chunk moves
/// only once both caps let it through at once ([`crate::cap::take_both`]); one the kernel refuses is carried, paid
/// for, into the fallback's first chunk. Under [`Interruptible`], a cancel stops the copy with [`Cancelled`] before
/// its next chunk. Uncapped, every chunk is `chunk` bytes, and nothing is charged.
pub(crate) fn copy_with(
    (from, src): (&File, &Tally),
    (to, dst): (&File, &Tally),
    chunk: usize,
    kernel: &mut dyn FnMut(&File, &File, usize) -> io::Result<InKernel>,
) -> io::Result<u64> {
    // Only the source's: a copy reads one Root, and a thread holds one reader permit at most.
    let _reading = src.reader();
    let mut copied = 0u64;
    // Where the copy started and the file ends, looked up only once a chunk is capped: the source's offset has moved
    // on by what was copied before then.
    let span = std::cell::OnceCell::new();
    let span = |copied: u64| {
        *span.get_or_init(|| {
            let mut from = from;
            let start = from.stream_position().unwrap_or(0).saturating_sub(copied);
            let end = from.metadata().map_or(u64::MAX, |m| m.len());
            (start, end)
        })
    };
    // The bytes to ask for, and the bytes to pay for them.
    let next = |copied: u64| -> (usize, u64) {
        let rate = [src.rate(Way::Read), dst.rate(Way::Write)]
            .into_iter()
            .flatten()
            .min();
        let Some(rate) = rate else {
            return (chunk, 0);
        };
        let (start, end) = span(copied);
        let at = start.saturating_add(copied);
        // Up to the next multiple of a chunk's size, so that the chunks after it start at one.
        let mut ask = copy_chunk(rate, chunk);
        let aligned = 1u64.checked_shl(at.trailing_zeros()).unwrap_or(u64::MAX);
        if aligned < ask as u64 && aligned >= crate::cap::MIN_PIECE as u64 {
            ask = aligned as usize;
        }
        let left = end.saturating_sub(at);
        (ask, left.min(ask as u64))
    };
    // Pays for a chunk of `n` bytes before it moves, at once in both caps: its receipts, to settle it.
    // Through both buckets where there are two, even while one is uncapped: a chunk waiting for one then still sees
    // the other's cap come back.
    let pay = |n: u64| -> io::Result<Paid> {
        let capped = [
            src.bucket(Way::Read).is_some(),
            dst.bucket(Way::Write).is_some(),
        ];
        let (a, b) = (src.cap(Way::Read), dst.cap(Way::Write));
        let receipts = match (a, b, src.0.as_deref(), dst.0.as_deref()) {
            (Some(a), Some(b), Some(sinks), _) => sinks.pay_both(a, b, n)?,
            (Some(a), None, Some(sinks), _) => [sinks.pay(a, n)?, None],
            (None, Some(b), _, Some(sinks)) => [None, sinks.pay(b, n)?],
            _ => [None, None],
        };
        // A change of rate while it waited may let fewer through.
        let n = receipts
            .iter()
            .flatten()
            .map(Receipt::bytes)
            .fold(n, u64::min);
        Ok(Paid {
            n,
            receipts,
            capped,
        })
    };
    // Counts the `n` bytes a chunk `paid` for moved, settling the difference.
    let moved = |paid: Paid, n: u64| {
        if n < paid.n {
            let back = paid.n - n;
            for ((tally, way), receipt) in [(src, Way::Read), (dst, Way::Write)]
                .into_iter()
                .zip(paid.receipts)
            {
                if let (Some(b), Some(sinks), Some(receipt)) =
                    (tally.bucket(way), &tally.0, receipt)
                {
                    b.refund(&receipt, back, sinks.clock().now());
                }
            }
        } else if n > paid.n {
            // The file grew since it was looked at: the bytes past its end then are paid for once they've moved, in
            // the caps that capped the chunk. Bytes that moved before a cap was set are not.
            for ((tally, way), capped) in [(src, Way::Read), (dst, Way::Write)]
                .into_iter()
                .zip(paid.capped)
            {
                if let (true, Some(b), Some(sinks)) = (capped, tally.bucket(way), &tally.0) {
                    sinks.pay_after(b, n - paid.n);
                }
            }
        }
        src.counted(Way::Read, n);
        dst.counted(Way::Write, n);
    };
    // A chunk paid for that the kernel refused, carried into the first chunk of the fallback rather than paid twice.
    let mut carried = loop {
        let (ask, n) = next(copied);
        let paid = pay(n)?;
        let ask = if paid.n < n { paid.n as usize } else { ask };
        match kernel(from, to, ask)? {
            // A first call that copies nothing may be a filesystem that copies nothing this way, as overlayfs once
            // did, rather than an empty file: the copy below tells them apart. Later, nothing is the end.
            InKernel::Copied(0) if copied == 0 => break Some((ask, paid)),
            InKernel::Copied(0) => {
                moved(paid, 0);
                return Ok(copied);
            }
            InKernel::Copied(n) => {
                moved(paid, n as u64);
                copied += n as u64;
            }
            InKernel::Refused => break Some((ask, paid)),
        }
    };
    // From where the kernel stopped, if it copied any: each chunk advanced both offsets. `io::copy` between two files
    // keeps std's own kernel copies, `sendfile` among them, before it falls back to a buffer.
    loop {
        let (ask, paid) = match carried.take() {
            Some(carried) => carried,
            None => {
                let (ask, n) = next(copied);
                let paid = pay(n)?;
                (if paid.n < n { paid.n as usize } else { ask }, paid)
            }
        };
        let reader = from;
        let mut writer = to;
        let n = io::copy(&mut reader.take(ask as u64), &mut writer)?;
        moved(paid, n);
        if n == 0 {
            return Ok(copied);
        }
        copied += n;
    }
}

/// A chunk of a copy paid for: its bytes, its receipt from each cap, the source's read cap first, and whether each
/// capped it.
#[derive(Clone, Copy)]
struct Paid {
    n: u64,
    receipts: [Option<Receipt>; 2],
    capped: [bool; 2],
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

    /// One pool of `limit` for every Root.
    #[derive(Debug)]
    struct OnePool(Arc<Pool>);

    impl RootLimits for OnePool {
        fn readers(&self, _: &str) -> Arc<Pool> {
            self.0.clone()
        }
    }

    fn shared(limit: usize) -> (Meter, Arc<Pool>) {
        let pool = Arc::new(Pool::new(limit));
        let meter = Meter::default();
        meter.share_limits(Arc::new(OnePool(pool.clone())));
        (meter, pool)
    }

    /// Runs `f` on its own thread, failing if it takes more than a few seconds.
    fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(f()));
        rx.recv_timeout(Duration::from_secs(5))
            .expect("it finished in time")
    }

    #[test]
    fn a_read_of_a_root_takes_one_of_its_readers_until_the_reader_drops() {
        let (meter, pool) = shared(1);
        let tally = meter.start(1, workers()).tally(Some("roms"));
        let mut reading = Reading::new(&b"payload"[..], &tally);
        assert_eq!(pool.taken(), 0, "nothing is taken before the first read");
        let mut buf = [0; 3];
        reading.read_exact(&mut buf).unwrap();
        reading.read_exact(&mut buf).unwrap();
        assert_eq!(pool.taken(), 1, "one permit for the whole file");
        drop(reading);
        assert_eq!(pool.taken(), 0);

        let scratch = meter.job(1).tally(None);
        let _unpooled = scratch.reader();
        assert_eq!(pool.taken(), 0, "scratch is no Root's");
    }

    #[test]
    fn a_copy_takes_a_reader_of_its_source_root_only() {
        let (meter, pool) = shared(1);
        let job = meter.start(1, workers());
        let (from, to) = (job.tally(Some("roms")), job.tally(Some("library")));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), b"bytes").unwrap();
        let source = File::open(dir.path().join("a")).unwrap();
        let dest = File::create(dir.path().join("b")).unwrap();
        let mut seen = 0;
        copy_with((&source, &from), (&dest, &to), 2, &mut |_, _, _| {
            seen = pool.taken();
            Ok(InKernel::Refused)
        })
        .unwrap();
        assert_eq!(
            seen, 1,
            "only the source's reader, even with both Roots in one pool"
        );
        assert_eq!(pool.taken(), 0);
    }

    #[test]
    fn a_thread_holding_a_reader_takes_no_second_one_and_never_waits_on_itself() {
        let (meter, pool) = shared(1);
        within(move || {
            let tally = meter.start(1, workers()).tally(Some("roms"));
            let outer = tally.reader();
            let inner = tally.reader();
            assert_eq!(pool.taken(), 1);
            drop(inner);
            assert!(
                ReaderPermit::on_this_thread(),
                "the outer permit still holds"
            );
            drop(outer);
            assert_eq!(pool.taken(), 0);
            assert!(!ReaderPermit::on_this_thread());
        });
    }

    #[test]
    fn a_reader_outliving_the_one_that_took_the_permit_keeps_it_until_it_drops() {
        let (meter, pool) = shared(1);
        within(move || {
            let tally = meter.start(1, workers()).tally(Some("roms"));
            let first = tally.reader();
            let second = tally.reader();
            let third = tally.reader();
            drop(first);
            drop(third);
            assert_eq!(pool.taken(), 1, "the second still reads under it");
            assert!(ReaderPermit::on_this_thread());
            drop(second);
            assert_eq!(pool.taken(), 0);
            assert!(!ReaderPermit::on_this_thread());
        });
    }

    #[test]
    fn a_thread_that_may_not_wait_reads_without_a_reader() {
        let (meter, pool) = shared(1);
        within(move || {
            let tally = meter.start(1, workers()).tally(Some("roms"));
            let _taken = pool.enter();
            let _no_waits = NoReaderWaits::new();
            let _none = tally.reader();
        });
    }

    #[test]
    fn a_reader_is_given_back_when_its_thread_panics_or_its_read_fails() {
        let (meter, pool) = shared(1);
        let job = meter.start(1, workers());
        let tally = job.tally(Some("roms"));
        let panicked = std::thread::spawn(move || {
            let _reading = tally.reader();
            panic!("a read that panics");
        })
        .join();
        assert!(panicked.is_err());
        assert_eq!(pool.taken(), 0);

        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("a read that fails"))
            }
        }
        let tally = job.tally(Some("roms"));
        let failed = within(move || {
            let mut buf = [0; 1];
            Reading::new(Failing, &tally).read(&mut buf).is_err()
        });
        assert!(failed);
        assert_eq!(pool.taken(), 0);
        let tally = job.tally(Some("roms"));
        within(move || drop(tally.reader()));
    }

    #[test]
    fn a_reader_waits_while_other_jobs_hold_every_reader_of_its_root() {
        let (meter, pool) = shared(1);
        let held = pool.clone();
        let permit = Pool::enter_owned(&held);
        let tally = meter.start(1, workers()).tally(Some("roms"));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _reading = tally.reader();
            tx.send(()).unwrap();
        });
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "it waits"
        );
        drop(permit);
        rx.recv_timeout(Duration::from_secs(5))
            .expect("it reads once a reader is free");
    }

    /// One pool of many readers, and one pair of byte-rate caps, for every Root.
    #[derive(Debug)]
    struct CapAll(Arc<Caps>);

    impl RootLimits for CapAll {
        fn readers(&self, _: &str) -> Arc<Pool> {
            Arc::new(Pool::new(64))
        }

        fn caps(&self, _: &str) -> Option<Arc<Caps>> {
            Some(self.0.clone())
        }
    }

    fn capped(read: Option<u64>, write: Option<u64>) -> (Meter, Arc<Caps>) {
        let caps = Arc::new(Caps::default());
        caps.set(read, write);
        let meter = Meter::default();
        meter.share_limits(Arc::new(CapAll(caps.clone())));
        (meter, caps)
    }

    const MIB: u64 = 1 << 20;

    /// The chunk sizes a copy of `len` bytes asks the kernel for, through tallies capped as `meter` caps them.
    fn chunks_asked(meter: &Meter, len: u32, chunk: usize) -> Vec<usize> {
        let dir = tempfile::tempdir().unwrap();
        let (bytes, from, to) = files(dir.path(), len);
        let job = meter.start(1, workers());
        let (src, dst) = (job.tally(Some("roms")), job.tally(Some("library")));
        let mut asked = Vec::new();
        copy_with((&from, &src), (&to, &dst), chunk, &mut |f, t, c| {
            asked.push(c);
            in_kernel(f, t, c)
        })
        .unwrap();
        assert_eq!(std::fs::read(dir.path().join("dst")).unwrap(), bytes);
        asked
    }

    #[test]
    fn unthrottled_nothing_is_charged_and_nothing_waits() {
        let (meter, caps) = capped(None, None);
        let tally = meter.start(1, workers()).tally(Some("roms"));
        for _ in 0..10_000 {
            tally.read(1 << 20);
            tally.wrote(1 << 20);
        }
        let asked = chunks_asked(&meter, 100_003, COPY_CHUNK);
        assert_eq!(
            asked[0], COPY_CHUNK,
            "uncapped, a copy keeps its full chunks"
        );
        // Reads and writes made through a tally are whole, and a cancel doesn't reach them: they check nothing but
        // the cap.
        let _interruptible = Interruptible::new();
        meter.cancel(1);
        let mut buf = vec![0; 8 << 20];
        let read = Reading::new(&[7u8; 8 << 20][..], &tally)
            .read(&mut buf)
            .unwrap();
        let wrote = Metered::new(io::sink(), &tally).write(&buf).unwrap();
        assert_eq!((read, wrote), (8 << 20, 8 << 20));
        assert!(caps.read.untouched() && caps.write.untouched());
        assert_eq!((caps.read.waited(), caps.write.waited()), (0, 0));
        assert_eq!(
            meter.root("roms").read,
            (10_000 << 20) + 100_003 + (8 << 20)
        );
    }

    #[test]
    fn a_capped_copy_charges_its_source_read_and_its_destination_write_in_smaller_chunks() {
        // 8 MiB/s: a quarter second is 2 MiB. The file fits in the burst, so nothing waits.
        let (meter, caps) = capped(Some(8 * MIB), None);
        assert_eq!(chunks_asked(&meter, 100_003, COPY_CHUNK)[0], 2 << 20);
        assert!(!caps.read.untouched(), "the source's read is charged");
        assert!(caps.write.untouched(), "no write cap");

        let (meter, caps) = capped(None, Some(400 * MIB));
        assert_eq!(chunks_asked(&meter, 100_003, COPY_CHUNK)[0], COPY_CHUNK);
        assert!(caps.read.untouched());
        assert!(
            !caps.write.untouched(),
            "the destination's write is charged"
        );
    }

    #[test]
    fn a_capped_copy_charges_only_what_is_left_of_the_file_and_settles_what_moved() {
        // 400 MiB/s: 100 ms of burst is 40 MiB. A file of 30 MiB is charged 30 MiB, not a 64 MiB chunk, and the copy's
        // last call, which finds the end of the file, is charged nothing and waits for nothing: by a clock standing
        // still, its 75 ms slot is still ahead. A second read right after waits only for what is over the burst.
        let (meter, caps, clock) = stepped(Some(400 * MIB), Some(400 * MIB));
        let len = 30 << 20;
        let meter2 = meter.clone();
        let asked = clock
            .clone()
            .spawn(move || {
                let asked = chunks_asked(&meter2, len, COPY_CHUNK);
                // Another 10 MiB fits what is left of the burst.
                meter2.job(1).tally(Some("roms")).read(9 << 20);
                asked
            })
            .join()
            .unwrap();
        assert_eq!(
            asked.len(),
            2,
            "a chunk, then the end of the file: {asked:?}"
        );
        assert_eq!(
            caps.read.waited(),
            0,
            "the copy was charged its bytes, not its chunk"
        );
        assert_eq!(caps.write.waited(), 0);
        assert_eq!(clock.now(), 0);
    }

    use crate::cap::lockstep::Lockstep;

    const SEC: u64 = 1_000_000_000;
    const MS: u64 = 1_000_000;

    /// Caps as `capped` sets them, waiting by a clock that moves on only once every thread on it is asleep.
    fn stepped(read: Option<u64>, write: Option<u64>) -> (Arc<Meter>, Arc<Caps>, Arc<Lockstep>) {
        let clock = Lockstep::new();
        let caps = Arc::new(Caps::with_clock(clock.clone()));
        caps.set(read, write);
        let meter = Meter::default();
        meter.share_limits(Arc::new(CapAll(caps.clone())));
        (Arc::new(meter), caps, clock)
    }

    /// What moved, and when, by the fake clock.
    type Log = Arc<Mutex<Vec<(u64, u64)>>>;

    /// The most bytes `log` holds within any `window` nanoseconds.
    fn most_within(log: &Log, window: u64) -> u64 {
        let log = log.lock().unwrap();
        log.iter()
            .map(|&(start, _)| {
                log.iter()
                    .filter(|(t, _)| (start..start + window).contains(t))
                    .map(|(_, n)| n)
                    .sum::<u64>()
            })
            .max()
            .unwrap_or(0)
    }

    fn moved_within(log: &Log, from: u64, to: u64) -> u64 {
        let log = log.lock().unwrap();
        log.iter()
            .filter(|(t, _)| (from..to).contains(t))
            .map(|(_, n)| n)
            .sum()
    }

    /// A file of `len` bytes, which logs when each read of it is made and what it asks for.
    struct Logged {
        clock: Arc<Lockstep>,
        log: Log,
        left: u64,
    }

    impl Read for Logged {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.left as usize);
            self.log.lock().unwrap().push((self.clock.now(), n as u64));
            self.left -= n as u64;
            Ok(n)
        }
    }

    #[test]
    fn concurrent_readers_read_nothing_the_cap_has_not_let_through() {
        // Sixteen readers of 64 KiB at 1 MiB/s: each read is paid for before it is made, so over any 5 s no more than
        // 5 MiB and the burst are read, where reading first and paying after reads one buffer of each reader more.
        let (meter, _caps, clock) = stepped(Some(MIB), None);
        let job = meter.start(1, workers());
        let log = Log::default();
        let readers: Vec<_> = (0..16)
            .map(|_| {
                let (tally, clock, log) = (job.tally(Some("roms")), clock.clone(), log.clone());
                clock.clone().spawn(move || {
                    let file = Logged {
                        clock: clock.clone(),
                        log,
                        left: u64::MAX,
                    };
                    let mut reading = Reading::new(file, &tally);
                    let mut buf = vec![0; 64 << 10];
                    while clock.now() < 12 * SEC {
                        assert!(reading.read(&mut buf).unwrap() > 0);
                    }
                })
            })
            .collect();
        for r in readers {
            r.join().unwrap();
        }
        let most = most_within(&log, 5 * SEC);
        assert!(most * 10 <= 5 * MIB * 11, "{most} bytes in 5 s");
        let all = moved_within(&log, 0, 12 * SEC);
        assert!(all * 10 >= 12 * MIB * 9, "only {all} bytes in 12 s");
    }

    #[test]
    fn a_capped_read_moves_at_most_a_quarter_second_of_the_cap_at_once() {
        let (meter, _caps, clock) = stepped(Some(MIB), None);
        let tally = meter.start(1, workers()).tally(Some("roms"));
        let log = Log::default();
        let file = Logged {
            clock: clock.clone(),
            log: log.clone(),
            left: 100,
        };
        clock
            .spawn(move || {
                let mut reading = Reading::new(file, &tally);
                let mut buf = vec![0; 1 << 20];
                assert_eq!(reading.read(&mut buf).unwrap(), 100, "a short read");
                assert_eq!(reading.read(&mut buf).unwrap(), 0);
            })
            .join()
            .unwrap();
        assert_eq!(log.lock().unwrap()[0].1, 100);
        let (meter, caps, clock) = stepped(Some(MIB), None);
        let tally = meter.start(1, workers()).tally(Some("roms"));
        let log = Log::default();
        let file = Logged {
            clock: clock.clone(),
            log: log.clone(),
            left: u64::MAX,
        };
        clock
            .spawn(move || {
                let mut buf = vec![0; 1 << 20];
                Reading::new(file, &tally).read(&mut buf).unwrap()
            })
            .join()
            .unwrap();
        // A quarter second's piece, of which the burst lets the first tenth through at once.
        assert_eq!(*log.lock().unwrap(), [(150 * MS, 256 << 10)]);
        assert!(!caps.read.untouched());
    }

    #[test]
    fn a_cancel_stops_staging_within_a_recheck_and_leaves_no_debt_to_the_next_job() {
        // A file of 20 MiB read at 1 MiB/s by a Job that can drop it, cancelled 2 s in.
        let (meter, caps, clock) = stepped(Some(MIB), None);
        let tally = meter.start(1, workers()).tally(Some("roms"));
        let log = Log::default();
        let file = Logged {
            clock: clock.clone(),
            log: log.clone(),
            left: 20 * MIB,
        };
        let (reader, canceller) = clock
            .clone()
            .spawn({
                let (clock, meter) = (clock.clone(), meter.clone());
                move || {
                    let reader = clock.clone().spawn(move || {
                        let interruptible = Interruptible::new();
                        let mut reading = Reading::new(file, &tally);
                        let mut buf = vec![0; 64 << 10];
                        loop {
                            match reading.read(&mut buf) {
                                Ok(0) => panic!("it read the whole file"),
                                Ok(_) => {}
                                Err(e) => {
                                    assert!(is_cancelled(&e), "{e}");
                                    assert!(interruptible.interrupted());
                                    return;
                                }
                            }
                        }
                    });
                    let canceller = {
                        let (clock, meter) = (clock.clone(), meter.clone());
                        clock.clone().spawn(move || {
                            clock.sleep(Duration::from_secs(2));
                            meter.cancel(1);
                            clock.now()
                        })
                    };
                    (reader, canceller)
                }
            })
            .join()
            .unwrap();
        let cancelled = canceller.join().unwrap();
        reader.join().unwrap();
        let last = log.lock().unwrap().last().unwrap().0;
        assert!(last <= cancelled + 100 * MS, "read until {last}");
        assert!(moved_within(&log, 0, u64::MAX) <= 3 * MIB);
        // The next Job's first piece goes within the burst the cancelled one already used.
        let next = meter.start(2, workers()).tally(Some("roms"));
        let started = clock.now();
        let file = Logged {
            clock: clock.clone(),
            log: log.clone(),
            left: u64::MAX,
        };
        let read_at = clock
            .clone()
            .spawn(move || {
                let read = Reading::new(file, &next).read(&mut [0; 64 << 10]).unwrap();
                assert_eq!(read, 64 << 10);
                clock.now()
            })
            .join()
            .unwrap();
        assert!(
            read_at - started <= 100 * MS,
            "waited {} ms",
            (read_at - started) / MS
        );
        assert!(caps.read.waited() > 0);
    }

    #[test]
    fn outside_staging_a_cancelled_job_still_waits_for_its_caps() {
        let (meter, _caps, clock) = stepped(Some(MIB), None);
        meter.start(1, workers());
        meter.cancel(1);
        let tally = meter.job(1).tally(Some("roms"));
        let log = Log::default();
        let file = Logged {
            clock: clock.clone(),
            log: log.clone(),
            left: 4 * MIB,
        };
        let done = clock
            .clone()
            .spawn(move || {
                io::copy(&mut Reading::new(file, &tally), &mut io::sink()).unwrap();
                clock.now()
            })
            .join()
            .unwrap();
        assert!(done >= 3 * SEC, "4 MiB at 1 MiB/s took {} ms", done / MS);
    }

    #[test]
    fn concurrent_copies_move_a_chunk_only_once_both_caps_let_it_through() {
        // Sixty-four copies from a source read at 8 MiB/s into a destination written at 1 MiB/s, whose write cap goes 3 s
        // in. Chunks paid for at the source while they waited for the destination must not go through at once then:
        // paying one and then waiting on the other moves 56 MiB in the 5 s from then.
        let (meter, caps, clock) = stepped(Some(8 * MIB), Some(MIB));
        let job = meter.start(1, workers());
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        File::create(&src).unwrap().set_len(1 << 30).unwrap();
        let log = Log::default();
        // Spawned from a thread on the clock, so that it doesn't move on before every one is.
        let spawning = clock.clone();
        let dir_path = dir.path().to_owned();
        let logs = log.clone();
        let (copies, uncap) = spawning
            .clone()
            .spawn(move || {
                let copies: Vec<_> = (0..64)
                    .map(|i| {
                        let from = File::open(&src).unwrap();
                        let to = File::create(dir_path.join(format!("dst{i}"))).unwrap();
                        let (from_tally, to_tally) =
                            (job.tally(Some("roms")), job.tally(Some("library")));
                        let (clock, log) = (clock.clone(), logs.clone());
                        clock.clone().spawn(move || {
                            let mut calls = 0;
                            copy_with(
                                (&from, &from_tally),
                                (&to, &to_tally),
                                COPY_CHUNK,
                                &mut |_, _, c| {
                                    calls += 1;
                                    let now = clock.now();
                                    if calls > 1 && now >= 10 * SEC {
                                        return Ok(InKernel::Copied(0));
                                    }
                                    log.lock().unwrap().push((now, c as u64));
                                    Ok(InKernel::Copied(c))
                                },
                            )
                            .unwrap()
                        })
                    })
                    .collect();
                let uncap = {
                    let clock = clock.clone();
                    clock.clone().spawn(move || {
                        clock.sleep(Duration::from_secs(3));
                        caps.set(Some(8 * MIB), None);
                    })
                };
                (copies, uncap)
            })
            .join()
            .unwrap();
        uncap.join().unwrap();
        for c in copies {
            c.join().unwrap();
        }
        let most = most_within(&log, 5 * SEC);
        assert!(most * 10 <= 8 * MIB * 5 * 11, "{} MiB in 5 s", most / MIB);
        let before = moved_within(&log, 0, 3 * SEC);
        assert!(
            before <= 3 * MIB + MIB,
            "{} KiB before the write cap went",
            before >> 10
        );
        let after = moved_within(&log, 4 * SEC, 10 * SEC);
        assert!(
            after * 10 >= 8 * MIB * 6 * 9,
            "only {} MiB after",
            after / MIB
        );
    }

    #[test]
    fn a_copy_the_kernel_refuses_reaches_its_cap() {
        // Copies of 256 KiB at 1 MiB/s, each refused by the kernel: the chunk paid for is carried into the fallback.
        let (meter, _caps, clock) = stepped(Some(MIB), None);
        let tally = meter.start(1, workers()).tally(Some("roms"));
        let dir = tempfile::tempdir().unwrap();
        let (bytes, _, _) = files(dir.path(), 256 << 10);
        let times = {
            let clock = clock.clone();
            let dir = dir.path().to_owned();
            clock.clone().spawn(move || {
                (0..9)
                    .map(|i| {
                        let from = File::open(dir.join("src")).unwrap();
                        let to = File::create(dir.join(format!("copy{i}"))).unwrap();
                        copy_with(
                            (&from, &tally),
                            (&to, &tally),
                            COPY_CHUNK,
                            &mut |_, _, _| Ok(InKernel::Refused),
                        )
                        .unwrap();
                        clock.now()
                    })
                    .collect::<Vec<_>>()
            })
        }
        .join()
        .unwrap();
        assert_eq!(std::fs::read(dir.path().join("copy8")).unwrap(), bytes);
        let rate = 8 * (256 << 10) * SEC / (times[8] - times[0]);
        assert!(rate * 10 >= MIB * 9, "{} KiB/s", rate >> 10);
    }

    #[test]
    fn a_thread_that_may_not_wait_is_charged_without_waiting() {
        let (meter, caps) = capped(Some(MIB), Some(MIB));
        let tally = meter.start(1, workers()).tally(Some("roms"));
        within(move || {
            let _writer = NoReaderWaits::new();
            tally.read(100 << 20);
            tally.wrote(100 << 20);
        });
        assert!(!caps.read.untouched() && !caps.write.untouched());
        assert_eq!((caps.read.waited(), caps.write.waited()), (0, 0));
    }

    #[test]
    fn what_another_process_moves_and_what_an_uncapped_tally_reads_are_counted_but_not_charged() {
        let (meter, caps) = capped(Some(MIB), Some(MIB));
        let job = meter.start(1, workers());
        let tally = job.tally(Some("roms"));
        tally.read_outside(100 << 20);
        tally.wrote_outside(100 << 20);
        job.tally(Some("roms")).uncapped().read(100 << 20);
        job.tally(None).read(100 << 20);
        assert!(caps.read.untouched() && caps.write.untouched());
        assert_eq!(meter.root("roms").read, 200 << 20);
    }

    /// The most bytes `log` holds within any `window` nanoseconds from `from` on.
    fn most_within_from(log: &Log, from: u64, window: u64) -> u64 {
        let later: Vec<_> = log
            .lock()
            .unwrap()
            .iter()
            .copied()
            .filter(|&(t, _)| t >= from)
            .collect();
        most_within(&Arc::new(Mutex::new(later)), window)
    }

    /// A kernel copy of a sparse file of `len` bytes that moves the source's offset along, and logs when each chunk
    /// moves and how big it is.
    fn sparse_kernel(
        clock: Arc<Lockstep>,
        log: Log,
        len: u64,
    ) -> impl FnMut(&File, &File, usize) -> io::Result<InKernel> {
        let mut left = len;
        move |mut from, _, c| {
            let n = (c as u64).min(left);
            from.seek(SeekFrom::Current(n as i64))?;
            left -= n;
            if n > 0 {
                log.lock().unwrap().push((clock.now(), n));
            }
            Ok(InKernel::Copied(n as usize))
        }
    }

    #[test]
    fn a_copy_gets_its_turn_among_readers_that_go_on_reading() {
        // Four readers of 64 KiB go on reading a root capped at 1 MiB/s while another job copies 2 MiB from it into a
        // root whose writes are capped at 1 MiB/s too, so there are always reads queued. The copy queues for its chunks as the readers do for their reads, so each
        // gets its turn and the copy finishes; waiting for the queue to empty first, it never would.
        let (meter, _caps, clock) = stepped(Some(MIB), Some(MIB));
        let (reads, copied) = (Log::default(), Log::default());
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        File::create(&src).unwrap().set_len(2 * MIB).unwrap();
        let (from, to) = (
            File::open(&src).unwrap(),
            File::create(dir.path().join("dst")).unwrap(),
        );
        let done = Arc::new(AtomicBool::new(false));
        let (readers, copy, watchdog) = clock
            .clone()
            .spawn({
                let (clock, meter, reads, copied, done) = (
                    clock.clone(),
                    meter.clone(),
                    reads.clone(),
                    copied.clone(),
                    done.clone(),
                );
                move || {
                    let reading = meter.start(1, workers());
                    let readers: Vec<_> = (0..4)
                        .map(|_| {
                            let (clock, done, tally) =
                                (clock.clone(), done.clone(), reading.tally(Some("roms")));
                            let file = Logged {
                                clock: clock.clone(),
                                log: reads.clone(),
                                left: u64::MAX,
                            };
                            clock.clone().spawn(move || {
                                let mut reading = Reading::new(file, &tally);
                                let mut buf = vec![0; 64 << 10];
                                while !done.load(Ordering::Relaxed) && clock.now() < 30 * SEC {
                                    let _ = reading.read(&mut buf).unwrap();
                                }
                            })
                        })
                        .collect();
                    let job = meter.start(2, workers());
                    let copy = clock.clone().spawn({
                        let clock = clock.clone();
                        move || {
                            let _interruptible = Interruptible::new();
                            let (src, dst) = (job.tally(Some("roms")), job.tally(Some("library")));
                            let copy = copy_with(
                                (&from, &src),
                                (&to, &dst),
                                COPY_CHUNK,
                                &mut sparse_kernel(clock.clone(), copied, 2 * MIB),
                            );
                            done.store(true, Ordering::Relaxed);
                            copy.map(|_| clock.now())
                        }
                    });
                    // Gives up on the copy after 30 s rather than hang.
                    let watchdog = clock.clone().spawn(move || {
                        clock.sleep(Duration::from_secs(30));
                        meter.cancel(2);
                    });
                    (readers, copy, watchdog)
                }
            })
            .join()
            .unwrap();
        let finished = copy.join().unwrap().expect("the copy finished");
        for r in readers {
            r.join().unwrap();
        }
        watchdog.join().unwrap();
        // They take turns in the queue: the copy a slot of 256 KiB, each reader one of 64 KiB. So the copy has half of
        // 1 MiB/s, and takes about 4 s.
        assert!(finished <= 5 * SEC, "the copy took {} ms", finished / MS);
        let read = moved_within(&reads, 0, finished);
        assert!(
            read * 4 >= finished * MIB / SEC,
            "the readers read only {} KiB in {} ms",
            read >> 10,
            finished / MS
        );
    }

    #[test]
    fn readers_that_find_the_ends_of_their_files_give_back_no_slot_others_queued_behind() {
        // Sixteen readers of files of 128 KiB, 64 KiB at a time, at 1 MiB/s: each reaches the end of its file about
        // 2 s in, with its next read paid for, and goes on to another. What is paid for and not read is never given
        // back from under the slots queued behind it, so the readers read no more than the cap and the burst.
        let (meter, _caps, clock) = stepped(Some(MIB), None);
        let job = meter.start(1, workers());
        let log = Log::default();
        let readers = clock
            .clone()
            .spawn({
                let (clock, log) = (clock.clone(), log.clone());
                move || {
                    (0..16)
                        .map(|_| {
                            let (tally, clock, log) =
                                (job.tally(Some("roms")), clock.clone(), log.clone());
                            clock.clone().spawn(move || {
                                let mut buf = vec![0; 64 << 10];
                                while clock.now() < 12 * SEC {
                                    let file = Logged {
                                        clock: clock.clone(),
                                        log: log.clone(),
                                        left: 128 << 10,
                                    };
                                    let mut reading = Reading::new(file, &tally);
                                    while reading.read(&mut buf).unwrap() > 0 {}
                                }
                            })
                        })
                        .collect::<Vec<_>>()
                }
            })
            .join()
            .unwrap();
        for r in readers {
            r.join().unwrap();
        }
        let most = most_within(&log, 5 * SEC);
        assert!(most * 10 <= 5 * MIB * 11, "{} KiB in 5 s", most >> 10);
        let all = moved_within(&log, 0, 12 * SEC);
        assert!(all * 10 >= 12 * MIB * 8, "only {} KiB in 12 s", all >> 10);
    }

    #[test]
    fn uncapping_and_capping_again_while_copies_wait_holds_each_cap() {
        // Sixteen copies from a source read at 8 MiB/s into a destination written at 1 MiB/s, whose write cap goes 3 s
        // in and comes back 6 s in. Slots reserved before the uncap aren't let through at once by it; chunks sized for
        // 8 MiB/s, waiting when the cap comes back, are not let through at 1 MiB/s whole.
        let (meter, caps, clock) = stepped(Some(8 * MIB), Some(MIB));
        let job = meter.start(1, workers());
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        File::create(&src).unwrap().set_len(1 << 30).unwrap();
        let log = Log::default();
        let (copies, changes) = clock
            .clone()
            .spawn({
                let (clock, log, dir) = (clock.clone(), log.clone(), dir.path().to_owned());
                move || {
                    let copies: Vec<_> = (0..16)
                        .map(|i| {
                            let from = File::open(&src).unwrap();
                            let to = File::create(dir.join(format!("dst{i}"))).unwrap();
                            let (src, dst) = (job.tally(Some("roms")), job.tally(Some("library")));
                            let (clock, log) = (clock.clone(), log.clone());
                            clock.clone().spawn(move || {
                                let mut kernel = sparse_kernel(clock.clone(), log, 1 << 30);
                                copy_with((&from, &src), (&to, &dst), COPY_CHUNK, &mut |f, t, c| {
                                    match clock.now() {
                                        now if now >= 12 * SEC => Ok(InKernel::Copied(0)),
                                        _ => kernel(f, t, c),
                                    }
                                })
                                .unwrap()
                            })
                        })
                        .collect();
                    let changes = clock.clone().spawn(move || {
                        clock.sleep(Duration::from_secs(3));
                        caps.set(Some(8 * MIB), None);
                        clock.sleep(Duration::from_secs(3));
                        caps.set(Some(8 * MIB), Some(MIB));
                    });
                    (copies, changes)
                }
            })
            .join()
            .unwrap();
        changes.join().unwrap();
        for c in copies {
            c.join().unwrap();
        }
        let before = moved_within(&log, 0, 3 * SEC);
        assert!(
            before * 10 <= 3 * MIB * 11 + MIB,
            "{} KiB before the uncap",
            before >> 10
        );
        let uncapped = most_within(&log, SEC);
        assert!(
            uncapped * 10 <= 8 * MIB * 14,
            "{} KiB in a second",
            uncapped >> 10
        );
        let after = most_within_from(&log, 6 * SEC, 5 * SEC);
        assert!(
            after * 10 <= 5 * MIB * 11,
            "{} KiB in 5 s after the cap came back",
            after >> 10
        );
        let moved = moved_within(&log, 6 * SEC, 12 * SEC);
        assert!(
            moved * 10 >= 6 * MIB * 8,
            "only {} KiB after the cap came back",
            moved >> 10
        );
    }

    #[test]
    fn a_copy_capped_halfway_pays_for_the_rest_before_it_moves() {
        // A copy of 128 MiB starts uncapped; its source is capped at 64 MiB/s once its first chunk of 64 MiB has moved.
        // The rest, 16 MiB at a time, is paid for before it moves, a quarter second apart.
        let (meter, caps, clock) = stepped(None, None);
        let tally = meter.start(1, workers()).tally(Some("roms"));
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        File::create(&src).unwrap().set_len(128 * MIB).unwrap();
        let (from, to) = (
            File::open(&src).unwrap(),
            File::create(dir.path().join("dst")).unwrap(),
        );
        let log = Log::default();
        clock
            .clone()
            .spawn({
                let log = log.clone();
                move || {
                    let mut kernel = sparse_kernel(clock.clone(), log, 128 * MIB);
                    let mut first = true;
                    let copied = copy_with(
                        (&from, &tally),
                        (&to, &tally),
                        COPY_CHUNK,
                        &mut |f, t, c| {
                            let copied = kernel(f, t, c);
                            if std::mem::take(&mut first) {
                                caps.set(Some(64 * MIB), None);
                            }
                            copied
                        },
                    )
                    .unwrap();
                    assert_eq!(copied, 128 * MIB);
                }
            })
            .join()
            .unwrap();
        let log = log.lock().unwrap();
        let after: Vec<_> = log[1..].iter().map(|&(t, n)| (t / MS, n / MIB)).collect();
        assert_eq!(after, [(150, 16), (400, 16), (650, 16), (900, 16)]);
    }
}
