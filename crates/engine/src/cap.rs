//! Byte-rate caps (ADR 0022): a read bucket and a write bucket for each Root, shared by every Job, that hold Title
//! Select's own reads and writes of the Root to the pace's share of its byte-rate Ceilings.
//!
//! A [`Bucket`] is a lock-free token bucket kept as one number, the time by which everything charged to it so far is
//! paid for at its rate (the "theoretical arrival time" of the generic cell rate algorithm). Charging `n` bytes moves
//! that time on by `n / rate`, from now if it had fallen behind, in one compare-and-swap; the charge then waits until
//! the time is no more than [`BURST`] ahead of now. So, over any span, a bucket lets through at most its rate times
//! the span plus [`BURST`], plus the one charge under way as the span starts, however many threads and Jobs share it:
//! nothing is leased to a thread, so nothing unspent can add up across threads.
//!
//! **Uncapped.** A bucket with no rate is uncapped: [`Bucket::capped`] is one relaxed load, and nothing else touches
//! the bucket. Under `unthrottled` every bucket is uncapped, so the hot path costs the meter's counter and that load.
//!
//! **Pace changes.** [`Bucket::set_rate`] takes effect at once for every later charge, and rescales what was already
//! charged ahead of now to the new rate. A charge waiting looks at the rate again at least every [`RECHECK`] and
//! rescales what it has left to wait the same way, so a slower pace slows even a single large file within a second,
//! and a faster one, or none, ends a wait within [`RECHECK`].
//!
//! **Waiting** sleeps, in slices of at most [`RECHECK`], and gives up, without waiting the rest, when its `stop` says
//! so: a cancelled Job's wait ends within [`RECHECK`], and what it didn't wait for is given back
//! ([`Bucket::refund`]), so a cancel leaves no debt. A refund gives back only time still to come, never time gone, so
//! it can't let a burst through. A wait holds no lock and nothing of the bucket, so a panic or an early return leaves
//! the bucket as it was, and nothing ever waits for a waiter: a bucket wait can't be part of a deadlock. It always
//! ends, since a bucket's rate is never zero while capped and what is ahead of now is bounded by what the threads
//! waiting charged. Holding a Root's reader permit while waiting on its read bucket is intended: the permit's reader
//! is the one being held back. The store's writer is never held while waiting: a thread holding it charges what it
//! reads and writes without waiting ([`crate::meter::NoReaderWaits`]). A 7z decoder waits on its read cap while it
//! holds its 7z gate permit, as it is the reader being held back; since every wait ends, the gate is held no longer
//! than the cap holds the decoder's reads.
//!
//! **Two caps at once.** A copy's chunk is paid for in two buckets, its source's reads and its destination's writes,
//! and moves only once both let it through at once ([`take_both`]).
//!
//! **Uncapping** forgets what was charged: a bucket capped again starts afresh, and waits on it end.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

/// How far ahead of now a bucket may be charged before a charge waits: what a bucket lets through at once after a
/// pause, and what it lets a charge go without a sleep too short to keep.
pub const BURST: Duration = Duration::from_millis(100);
/// The longest a wait sleeps before it looks at the rate, and its stop, again.
pub const RECHECK: Duration = Duration::from_millis(100);

const NS: u128 = 1_000_000_000;

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// The nanoseconds `n` bytes take at `rate` bytes a second.
fn cost(n: u64, rate: u64) -> u64 {
    u64::try_from(u128::from(n) * NS / u128::from(rate.max(1))).unwrap_or(u64::MAX)
}

/// `ahead` nanoseconds at `from` bytes a second, at `to` bytes a second instead.
fn rescale(ahead: u64, from: u64, to: u64) -> u64 {
    u64::try_from(u128::from(ahead) * u128::from(from) / u128::from(to.max(1))).unwrap_or(u64::MAX)
}

/// Where time is read and slept: the real monotonic clock, or a fake one in tests.
pub trait Clock {
    /// Nanoseconds since some fixed moment.
    fn now(&self) -> u64;
    fn sleep(&self, d: Duration);
}

/// The process's monotonic clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct Monotonic;

impl Clock for Monotonic {
    fn now(&self) -> u64 {
        static EPOCH: OnceLock<Instant> = OnceLock::new();
        nanos(EPOCH.get_or_init(Instant::now).elapsed())
    }

    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// One byte-rate cap, shared by every Job reading or writing one Root.
#[derive(Debug, Default)]
pub struct Bucket {
    /// Bytes a second; 0 while uncapped.
    rate: AtomicU64,
    /// When, in [`Clock::now`]'s nanoseconds, everything charged so far is paid for.
    paid_until: AtomicU64,
    /// Moved on each time the bucket is capped afresh or uncapped, when what was charged before is forgotten: a wait
    /// on a charge of an earlier epoch ends, and a refund of one gives nothing back.
    epoch: AtomicU64,
    /// How many charges have waited, for tests.
    waits: AtomicU64,
}

impl Bucket {
    /// Whether the bucket holds anything back: one relaxed load.
    #[inline]
    pub fn capped(&self) -> bool {
        self.rate.load(Ordering::Relaxed) != 0
    }

    /// Its rate, in bytes a second; none while uncapped.
    pub fn rate(&self) -> Option<u64> {
        Some(self.rate.load(Ordering::Relaxed)).filter(|&r| r != 0)
    }

    /// Caps the bucket at `rate` bytes a second, or uncaps it, from `now` on. From one rate to another, what was
    /// charged ahead of now is rescaled to the new rate; uncapped, or capped afresh, nothing charged before is owed.
    pub fn set_rate(&self, rate: Option<u64>, now: u64) {
        let rate = rate.unwrap_or(0);
        let was = self.rate.swap(rate, Ordering::Relaxed);
        if was == rate {
            return;
        }
        // A charge that read the old rate just before the swap may land after this; it is off by that one charge.
        if was == 0 || rate == 0 {
            self.epoch.fetch_add(1, Ordering::Relaxed);
            self.paid_until.store(0, Ordering::Relaxed);
            return;
        }
        let _ = self
            .paid_until
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |paid| {
                (paid > now).then(|| now.saturating_add(rescale(paid - now, was, rate)))
            });
    }

    /// The epoch charges made now belong to, for [`Bucket::refund`].
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// How far ahead of `now`, in nanoseconds, it is charged: none while uncapped.
    fn backlog(&self, now: u64) -> u64 {
        match self.capped() {
            true => self.paid_until.load(Ordering::Relaxed).saturating_sub(now),
            false => 0,
        }
    }

    /// Charges `n` bytes at `now`: what is left to wait, if anything, before they are paid for. Nothing while
    /// uncapped, and then the bucket isn't touched.
    pub fn charge(&self, n: u64, now: u64) -> Option<Debt> {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 || n == 0 {
            return None;
        }
        let epoch = self.epoch.load(Ordering::Relaxed);
        let cost = cost(n, rate);
        let mut paid = self.paid_until.load(Ordering::Relaxed);
        let until = loop {
            let next = paid.max(now).saturating_add(cost);
            match self.paid_until.compare_exchange_weak(
                paid,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break next.saturating_sub(nanos(BURST)),
                Err(now_paid) => paid = now_paid,
            }
        };
        (until > now).then_some(Debt { until, rate, epoch })
    }

    /// Gives back, at `now`, `n` bytes charged in `epoch` but not moved, as by a read that found the end of its file
    /// first. Only what is still ahead of now is given back: time already gone can't be spent again, so a refund
    /// never lets anything through that the cap would not have.
    pub fn refund(&self, n: u64, now: u64, epoch: u64) {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 || n == 0 || self.epoch() != epoch {
            return;
        }
        let cost = cost(n, rate);
        let _ = self
            .paid_until
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |paid| {
                Some(paid.saturating_sub(cost).max(now.min(paid)))
            });
    }

    /// Charges `n` bytes and waits, by `clock`, until they are paid for; `false`, with what wasn't waited for given
    /// back, if `stop` says to give up first.
    pub fn take(&self, n: u64, clock: &(impl Clock + ?Sized), stop: &dyn Fn() -> bool) -> bool {
        let epoch = self.epoch();
        let Some(debt) = self.charge(n, clock.now()) else {
            return true;
        };
        if self.wait(debt, clock, stop) {
            return true;
        }
        self.refund(n, clock.now(), epoch);
        false
    }

    /// Waits out `debt`, by `clock`: `false` if `stop` says to give up first.
    pub fn wait(
        &self,
        mut debt: Debt,
        clock: &(impl Clock + ?Sized),
        stop: &dyn Fn() -> bool,
    ) -> bool {
        self.waits.fetch_add(1, Ordering::Relaxed);
        while let Some(d) = debt.poll(self, clock.now()) {
            if stop() {
                return false;
            }
            clock.sleep(d);
        }
        true
    }

    /// How many charges have waited on it.
    pub fn waited(&self) -> u64 {
        self.waits.load(Ordering::Relaxed)
    }

    /// Whether nothing is charged to it, since it was last capped.
    pub fn untouched(&self) -> bool {
        self.paid_until.load(Ordering::Relaxed) == 0
    }
}

/// How long a chunk one bucket has let through may wait for the other's, when it is charged to two.
pub const GRACE: Duration = Duration::from_millis(50);

/// Charges `n` bytes to both `a` and `b`, as a copy's chunk is to its source's read cap and its destination's write
/// cap, and waits, by `clock`, until both let it through at once; `false` if `stop` says to give up first.
///
/// A payment to one bucket that then waits its turn in the other would fall into the past and, with every other
/// chunk waiting the same way, go through in a burst once the other let them all go. So nothing is charged while the
/// chunk waits its turn: it sleeps until neither bucket is charged more than [`GRACE`] ahead of now, then charges both
/// at that moment and waits out both. If another chunk took the turn first, both charges are given back whole, as
/// they are still ahead of now, and it sleeps again. A payment then waits at most the other bucket's charge for the
/// same piece and [`GRACE`] before its bytes move: a quarter second and [`GRACE`] at the lower cap, as a copy's pieces
/// are sized to it, so over any span each bucket lets through no more than it would alone and about that much of its
/// rate. Chunks waiting this way aren't served in order: one may lose its turn more than once, but a turn lost costs
/// neither bucket anything, and the caps are used in full.
pub fn take_both(
    a: &Bucket,
    b: &Bucket,
    n: u64,
    clock: &(impl Clock + ?Sized),
    stop: &dyn Fn() -> bool,
) -> bool {
    let grace = nanos(GRACE);
    let mut waited = false;
    loop {
        let now = clock.now();
        let backlog = a.backlog(now).max(b.backlog(now));
        if backlog > grace {
            if stop() {
                return false;
            }
            if !waited {
                waited = true;
                for bucket in [a, b] {
                    if bucket.backlog(now) > grace {
                        bucket.waits.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            clock.sleep(Duration::from_nanos(backlog - grace).min(RECHECK));
            continue;
        }
        let epochs = (a.epoch(), b.epoch());
        let debts = (a.charge(n, now), b.charge(n, now));
        // Charged further ahead than the backlog let through: another chunk took the turn in between.
        let late = |debt: Option<Debt>| {
            debt.is_some_and(|d| {
                d.until.saturating_add(nanos(BURST)) > now + grace + cost(n, d.rate)
            })
        };
        if late(debts.0) || late(debts.1) {
            a.refund(n, now, epochs.0);
            b.refund(n, now, epochs.1);
            continue;
        }
        let paid = debts.0.is_none_or(|d| a.wait(d, clock, stop))
            && debts.1.is_none_or(|d| b.wait(d, clock, stop));
        if !paid {
            let now = clock.now();
            a.refund(n, now, epochs.0);
            b.refund(n, now, epochs.1);
        }
        return paid;
    }
}

/// What one charge has left to wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Debt {
    /// When it is paid for, at `rate`.
    until: u64,
    rate: u64,
    /// The bucket's epoch it was charged in.
    epoch: u64,
}

impl Debt {
    /// How long to sleep at `now` before looking again, at most [`RECHECK`]; none once paid for, once `bucket` is
    /// uncapped, or once it was capped afresh. A change of rate since the last look rescales what is left.
    pub fn poll(&mut self, bucket: &Bucket, now: u64) -> Option<Duration> {
        let rate = bucket.rate.load(Ordering::Relaxed);
        if rate == 0 || bucket.epoch() != self.epoch {
            return None;
        }
        if rate != self.rate {
            if self.until > now {
                self.until = now.saturating_add(rescale(self.until - now, self.rate, rate));
            }
            self.rate = rate;
        }
        (self.until > now).then(|| Duration::from_nanos(self.until - now).min(RECHECK))
    }

    /// What is left to wait at `now`, at the rate it was charged at.
    pub fn left(&self, now: u64) -> Duration {
        Duration::from_nanos(self.until.saturating_sub(now))
    }
}

/// A Root's two caps, its reads and its writes, and the clock they wait by.
pub struct Caps {
    pub read: Bucket,
    pub write: Bucket,
    clock: Arc<dyn Clock + Send + Sync>,
}

impl Default for Caps {
    fn default() -> Caps {
        Caps::with_clock(Arc::new(Monotonic))
    }
}

impl std::fmt::Debug for Caps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Caps")
            .field("read", &self.read)
            .field("write", &self.write)
            .finish_non_exhaustive()
    }
}

impl Caps {
    /// Uncapped caps that wait by `clock`: a fake one, in tests.
    pub fn with_clock(clock: Arc<dyn Clock + Send + Sync>) -> Caps {
        Caps {
            read: Bucket::default(),
            write: Bucket::default(),
            clock,
        }
    }

    /// Caps reads at `read` and writes at `write` bytes a second, or uncaps either, from now on.
    pub fn set(&self, read: Option<u64>, write: Option<u64>) {
        let now = self.clock.now();
        self.read.set_rate(read, now);
        self.write.set_rate(write, now);
    }

    /// The clock its waits go by.
    pub fn clock(&self) -> &(dyn Clock + Send + Sync) {
        &*self.clock
    }
}

/// The smallest piece a capped read, write or copy moves at once.
pub const MIN_PIECE: usize = 64 << 10;
/// The share of a second's bytes at the cap a capped read, write or copy moves at once.
const PIECE: Duration = Duration::from_millis(250);

/// The most bytes a read or write capped at `rate` bytes a second moves at once: a quarter second's worth, and no
/// less than [`MIN_PIECE`]. Each piece is paid for before it moves, so a quarter second keeps a piece from bursting
/// past the cap over a few seconds, even at a cap of a few hundred KiB/s.
pub fn piece(rate: u64) -> usize {
    let fits = u128::from(rate) * PIECE.as_nanos() / NS;
    usize::try_from(fits).unwrap_or(usize::MAX).max(MIN_PIECE)
}

/// The bytes a copy capped at `rate` bytes a second asks the kernel for at once, at most `most`: the largest power of
/// two no more than a [`piece`]. A power of two from the start of the file keeps each chunk aligned to every block
/// size up to its own, so a filesystem that clones blocks still can.
pub fn copy_chunk(rate: u64, most: usize) -> usize {
    let fits = piece(rate);
    let pow = 1usize << (usize::BITS - 1 - fits.leading_zeros());
    pow.clamp(MIN_PIECE.min(most), most)
}

/// A fake clock for tests with several threads: time moves on only once every thread it runs is asleep, to the
/// earliest time one of them wakes. So each thread sees time stand still while it works, and a test's result is
/// the same however the threads are scheduled.
#[cfg(test)]
pub(crate) mod lockstep {
    use std::sync::{Arc, Condvar, Mutex, PoisonError};
    use std::time::Duration;

    use super::{Clock, nanos};

    #[derive(Default)]
    struct State {
        now: u64,
        running: usize,
        /// Each thread asleep: when it wakes, and its ticket.
        asleep: Vec<(u64, u64)>,
        woken: Vec<u64>,
        tickets: u64,
    }

    #[derive(Default)]
    pub(crate) struct Lockstep {
        state: Mutex<State>,
        woke: Condvar,
    }

    impl Lockstep {
        pub(crate) fn new() -> Arc<Lockstep> {
            Arc::default()
        }

        /// Runs `f` on a thread of its own on this clock: time doesn't move on while it works.
        pub(crate) fn spawn<T: Send + 'static>(
            self: &Arc<Self>,
            f: impl FnOnce() -> T + Send + 'static,
        ) -> std::thread::JoinHandle<T> {
            self.lock().running += 1;
            let clock = self.clone();
            std::thread::spawn(move || {
                struct Done(Arc<Lockstep>);
                impl Drop for Done {
                    fn drop(&mut self) {
                        let mut state = self.0.lock();
                        state.running -= 1;
                        self.0.advance(&mut state);
                    }
                }
                let _done = Done(clock);
                f()
            })
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, State> {
            self.state.lock().unwrap_or_else(PoisonError::into_inner)
        }

        /// Once every thread is asleep, moves time on to the earliest wake and wakes every thread due.
        fn advance(&self, state: &mut State) {
            if state.running > 0 || state.asleep.is_empty() {
                return;
            }
            let next = state.asleep.iter().map(|(at, _)| *at).min().expect("one");
            state.now = state.now.max(next);
            let now = state.now;
            let (due, later) = state.asleep.drain(..).partition(|(at, _)| *at <= now);
            state.asleep = later;
            for (_, ticket) in due {
                state.running += 1;
                state.woken.push(ticket);
            }
            self.woke.notify_all();
        }
    }

    impl Clock for Lockstep {
        fn now(&self) -> u64 {
            self.lock().now
        }

        fn sleep(&self, d: Duration) {
            let mut state = self.lock();
            let ticket = state.tickets;
            state.tickets += 1;
            let at = state.now + nanos(d).max(1);
            state.asleep.push((at, ticket));
            state.running -= 1;
            self.advance(&mut state);
            loop {
                if let Some(i) = state.woken.iter().position(|t| *t == ticket) {
                    state.woken.swap_remove(i);
                    return;
                }
                state = self
                    .woke
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    use super::*;

    const MIB: u64 = 1 << 20;
    const SEC: u64 = 1_000_000_000;
    const MS: u64 = 1_000_000;

    fn capped(mib_per_s: u64) -> Bucket {
        let b = Bucket::default();
        b.set_rate(Some(mib_per_s * MIB), 0);
        b
    }

    /// One reader of a simulated run: it moves `chunk` bytes at a time, each taking `work` nanoseconds of its own.
    #[derive(Clone, Copy)]
    struct Reader {
        chunk: u64,
        work: u64,
    }

    /// Events by time, and in the order they were queued at one time.
    #[derive(Default)]
    struct Queue {
        heap: BinaryHeap<Reverse<(u64, u64, usize)>>,
        events: Vec<Option<Event>>,
    }

    impl Queue {
        fn push(&mut self, at: u64, e: Event) {
            self.events.push(Some(e));
            let seq = self.events.len() - 1;
            self.heap.push(Reverse((at, seq as u64, seq)));
        }

        fn pop(&mut self) -> Option<(u64, Event)> {
            let Reverse((at, _, i)) = self.heap.pop()?;
            Some((at, self.events[i].take().unwrap()))
        }
    }

    enum Event {
        Ready(usize),
        Waiting(usize, Debt),
        Rate(Option<u64>),
    }

    /// Runs `readers` against `bucket` for `span` nanoseconds of simulated time, every one on the fake clock, and
    /// with the rate changes `rates` (at a time, to a rate); returns each chunk let through, by when.
    fn simulate(
        bucket: &Bucket,
        readers: &[Reader],
        rates: &[(u64, Option<u64>)],
        span: u64,
    ) -> Vec<(u64, u64)> {
        let mut queue = Queue::default();
        for (i, _) in readers.iter().enumerate() {
            queue.push(i as u64, Event::Ready(i));
        }
        for &(at, rate) in rates {
            queue.push(at, Event::Rate(rate));
        }
        let mut through = Vec::new();
        while let Some((now, e)) = queue.pop() {
            if now > span {
                break;
            }
            let (i, debt) = match e {
                Event::Rate(rate) => {
                    bucket.set_rate(rate, now);
                    continue;
                }
                Event::Ready(i) => (i, bucket.charge(readers[i].chunk, now)),
                Event::Waiting(i, debt) => (i, Some(debt)),
            };
            // A charge that has to wait looks again when its sleep ends; one paid for goes through now.
            if let Some(mut debt) = debt
                && let Some(d) = debt.poll(bucket, now)
            {
                queue.push(now + nanos(d), Event::Waiting(i, debt));
                continue;
            }
            through.push((now, readers[i].chunk));
            queue.push(now + readers[i].work, Event::Ready(i));
        }
        through
    }

    /// The most bytes let through within any `window` from `from` on.
    fn most_within(through: &[(u64, u64)], window: u64, from: u64) -> u64 {
        through
            .iter()
            .filter(|(t, _)| *t >= from)
            .map(|&(start, _)| {
                through
                    .iter()
                    .filter(|(t, _)| *t >= start && *t < start + window)
                    .map(|(_, n)| n)
                    .sum::<u64>()
            })
            .max()
            .unwrap_or(0)
    }

    fn within(through: &[(u64, u64)], from: u64, to: u64) -> u64 {
        through
            .iter()
            .filter(|(t, _)| (from..to).contains(t))
            .map(|(_, n)| n)
            .sum()
    }

    /// Several Jobs' threads on one Root capped at `rate`: payload reads of 64 KiB and 1 MiB, and copy chunks, each
    /// as fast as the disk would go.
    fn several_jobs(rate: u64) -> Vec<Reader> {
        let mut readers = Vec::new();
        for _ in 0..6 {
            readers.push(Reader {
                chunk: 64 << 10,
                work: 20_000,
            });
        }
        for _ in 0..4 {
            readers.push(Reader {
                chunk: MIB,
                work: MS,
            });
        }
        for _ in 0..2 {
            readers.push(Reader {
                chunk: copy_chunk(rate, 64 << 20) as u64,
                work: 0,
            });
        }
        readers
    }

    #[test]
    fn a_cap_holds_several_jobs_on_one_root_within_ten_percent_over_any_five_seconds() {
        for mib_per_s in [8, 25, 100, 200, 400, 1000] {
            let bucket = capped(mib_per_s);
            let through = simulate(&bucket, &several_jobs(mib_per_s * MIB), &[], 30 * SEC);
            let cap = mib_per_s * MIB * 5;
            let most = most_within(&through, 5 * SEC, 0);
            assert!(
                most * 10 <= cap * 11,
                "{mib_per_s} MiB/s: {} MiB in 5 s",
                most / MIB
            );
            // And it isn't held far under the cap.
            let all = within(&through, 0, 30 * SEC);
            assert!(
                all * 10 >= mib_per_s * MIB * 30 * 9,
                "{mib_per_s} MiB/s: only {} MiB in 30 s",
                all / MIB
            );
        }
    }

    #[test]
    fn from_rest_a_bucket_lets_through_no_more_than_the_burst_at_once() {
        let bucket = capped(100);
        let readers = [Reader {
            chunk: MIB,
            work: 0,
        }];
        // A reader as fast as it can go, from rest.
        let through = simulate(&bucket, &readers, &[], 2 * SEC);
        let first = through.iter().take_while(|(t, _)| *t == 0).count() as u64;
        assert!(first * MIB <= 100 * MIB / 10 + MIB, "{first} MiB at once");
    }

    #[test]
    fn a_slower_pace_reaches_a_file_already_being_read_within_a_second() {
        // One file read a MiB at a time at the default pace's 200 MiB/s, then gentle's 100 MiB/s from 3 s.
        let bucket = capped(200);
        let reader = [Reader {
            chunk: MIB,
            work: 0,
        }];
        let through = simulate(&bucket, &reader, &[(3 * SEC, Some(100 * MIB))], 8 * SEC);
        let before = within(&through, 2 * SEC, 3 * SEC);
        assert!(before >= 190 * MIB, "{} MiB/s before", before / MIB);
        for from in [4 * SEC, 5 * SEC, 6 * SEC] {
            let after = within(&through, from, from + SEC);
            assert!(
                (95 * MIB..=110 * MIB).contains(&after),
                "{} MiB/s a second after",
                after / MIB
            );
        }
        // Already within the first second after the change, the file is read at most the old rate's burst faster.
        let first = within(&through, 3 * SEC, 4 * SEC);
        assert!(first <= 100 * MIB + 20 * MIB + MIB, "{}", first / MIB);
    }

    #[test]
    fn a_pace_change_reaches_a_charge_already_waiting() {
        let chunk = [Reader {
            chunk: 64 * MIB,
            work: 10 * SEC,
        }];
        let first = |mib_per_s, rates: &[(u64, Option<u64>)]| {
            simulate(&capped(mib_per_s), &chunk, rates, 5 * SEC)[0].0
        };
        // A 64 MiB copy chunk at 16 MiB/s waits 3.9 s; the rate quadruples a second in, and the 2.9 s left go four
        // times as fast.
        let faster = first(16, &[(SEC, Some(64 * MIB))]);
        assert_eq!(faster, SEC + 2_900 * MS / 4);
        // At 64 MiB/s it waits 0.9 s; the rate quarters half a second in, and the 0.4 s left take four times as long.
        let slower = first(64, &[(SEC / 2, Some(16 * MIB))]);
        assert_eq!(slower, SEC / 2 + 4 * 400 * MS);
        // Uncapped while it waits, it goes within a recheck.
        let uncapped = first(64, &[(SEC / 2, Some(16 * MIB)), (SEC, None)]);
        assert!(
            (SEC..=SEC + nanos(RECHECK)).contains(&uncapped),
            "{} ms",
            uncapped / MS
        );
    }

    #[test]
    fn an_uncapped_bucket_is_never_charged_and_never_waits() {
        let bucket = Bucket::default();
        assert!(!bucket.capped());
        assert_eq!(bucket.charge(64 * MIB, 5), None);
        let through = simulate(
            &bucket,
            &[Reader {
                chunk: MIB,
                work: MS,
            }],
            &[],
            SEC,
        );
        assert_eq!(through.len(), 1001, "every millisecond from 0 to 1 s");
        assert!(bucket.untouched());
        assert_eq!(bucket.waited(), 0);
    }

    /// Something to run at a given time, in nanoseconds.
    type Hook<'a> = (u64, Box<dyn FnOnce() + 'a>);

    /// A clock that moves only when slept on, and runs `at` once its time comes.
    struct Fake<'a> {
        now: Cell<u64>,
        slept: RefCell<Vec<Duration>>,
        at: RefCell<Vec<Hook<'a>>>,
    }

    impl<'a> Fake<'a> {
        fn new() -> Fake<'a> {
            Fake {
                now: Cell::new(0),
                slept: RefCell::default(),
                at: RefCell::default(),
            }
        }
    }

    impl Clock for Fake<'_> {
        fn now(&self) -> u64 {
            self.now.get()
        }

        fn sleep(&self, d: Duration) {
            self.slept.borrow_mut().push(d);
            self.now.set(self.now.get() + nanos(d));
            let now = self.now.get();
            let due: Vec<_> = {
                let mut at = self.at.borrow_mut();
                let (due, later) = at.drain(..).partition(|(t, _)| *t <= now);
                *at = later;
                due
            };
            for (_, f) in due {
                f();
            }
        }
    }

    #[test]
    fn a_wait_sleeps_in_slices_and_gives_up_when_stopped() {
        let bucket = capped(10);
        let clock = Fake::new();
        // 10 MiB at 10 MiB/s is a second, less the burst.
        assert!(bucket.take(10 * MIB, &clock, &|| false));
        assert_eq!(clock.now(), 900 * MS);
        assert!(clock.slept.borrow().iter().all(|d| *d <= RECHECK));
        assert_eq!(bucket.waited(), 1);

        // A stop asked for half a second into a ten-second wait ends it at the next slice.
        let stopped = Cell::new(false);
        let clock = Fake::new();
        clock.now.set(900 * MS);
        clock
            .at
            .borrow_mut()
            .push((1_400 * MS, Box::new(|| stopped.set(true))));
        assert!(!bucket.take(100 * MIB, &clock, &|| stopped.get()));
        assert!(
            clock.now() <= 1_400 * MS + nanos(RECHECK),
            "{}",
            clock.now()
        );
        // What it didn't wait for is given back: the next charge owes nothing of it.
        assert_eq!(bucket.charge(MIB, clock.now()), None);
    }

    #[test]
    fn a_wait_follows_a_pace_change_made_while_it_sleeps() {
        let bucket = capped(10);
        let clock = Fake::new();
        // A 20 MiB charge at 10 MiB/s, uncapped half a second in.
        clock
            .at
            .borrow_mut()
            .push((500 * MS, Box::new(|| bucket.set_rate(None, 500 * MS))));
        bucket.take(20 * MIB, &clock, &|| false);
        assert_eq!(clock.now(), 500 * MS);
    }

    #[test]
    fn a_refund_gives_back_what_a_copy_did_not_use() {
        let bucket = capped(10);
        // 10 MiB charged for a chunk, of which the copy moved half a MiB.
        assert!(bucket.charge(10 * MIB, 0).is_some());
        bucket.refund(19 * MIB / 2, 0, bucket.epoch());
        assert_eq!(bucket.charge(MIB / 2, 0), None, "within the burst again");
    }

    #[test]
    fn a_refund_gives_back_only_time_still_to_come() {
        let bucket = capped(1);
        let epoch = bucket.epoch();
        // 2 MiB at 1 MiB/s is paid for at 2 s; half a MiB of it is given back at 1.9 s, when only 0.1 s is still to
        // come.
        assert!(bucket.charge(2 * MIB, 0).is_some());
        bucket.refund(MIB / 2, 1_900 * MS, epoch);
        assert_eq!(bucket.paid_until.load(Ordering::Relaxed), 1_900 * MS);
        // Charged before the bucket was capped afresh, nothing is given back.
        bucket.set_rate(None, 2 * SEC);
        bucket.set_rate(Some(MIB), 2 * SEC);
        assert!(bucket.charge(MIB, 2 * SEC).is_some());
        bucket.refund(MIB, 2 * SEC, epoch);
        assert_eq!(bucket.paid_until.load(Ordering::Relaxed), 3 * SEC);
    }

    #[test]
    fn uncapping_forgets_what_was_charged_at_the_old_rate() {
        // 4 MiB charged at 1 MiB/s, uncapped at 0.1 s, then capped at 100 MiB/s at 0.2 s: a MiB then fits the burst,
        // rather than waiting out the 3.7 s still owed at the old rate.
        let bucket = capped(1);
        let mut owed = bucket.charge(4 * MIB, 0).expect("over the burst");
        bucket.set_rate(None, 100 * MS);
        bucket.set_rate(Some(100 * MIB), 200 * MS);
        assert_eq!(bucket.charge(MIB, 200 * MS), None);
        assert_eq!(
            owed.poll(&bucket, 200 * MS),
            None,
            "a wait from before ends"
        );
    }

    #[test]
    fn a_capped_copy_moves_a_power_of_two_of_at_most_a_quarter_second_at_the_cap() {
        let most = 64 << 20;
        assert_eq!(copy_chunk(400 * MIB, most), 64 << 20);
        assert_eq!(copy_chunk(200 * MIB, most), 32 << 20);
        assert_eq!(copy_chunk(100 * MIB, most), 16 << 20);
        assert_eq!(copy_chunk(50 * MIB, most), 8 << 20);
        assert_eq!(copy_chunk(MIB, most), 256 << 10);
        assert_eq!(copy_chunk(MIB / 8, most), 64 << 10, "never under 64 KiB");
        assert_eq!(copy_chunk(400 * MIB, 4096), 4096, "never over the most");
        assert_eq!(piece(MIB), 256 << 10);
        assert_eq!(piece(MIB / 8), 64 << 10);
    }
}
