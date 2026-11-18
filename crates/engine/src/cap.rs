//! Byte-rate caps (ADR 0022): a read bucket and a write bucket for each Root, shared by every Job, that hold Title
//! Select's own reads and writes of the Root to the pace's share of its byte-rate Ceilings.
//!
//! A [`Bucket`] is a queue of reservations kept as one number, the time by which everything reserved so far is paid
//! for at its rate (the "theoretical arrival time" of the generic cell rate algorithm), under a lock held only for the
//! arithmetic of a reservation, never across a wait. Reserving `n` bytes takes the slot of `n / rate` from that time,
//! or from now if it had fallen behind, and hands back a [`Receipt`] for it; the bytes may move once the slot ends no more than [`BURST`] after now. So
//! reservations are served in the order they were taken, a later one never overtaking an earlier one, and no thread
//! can be passed over for ever: each waits only for the slots taken before its own.
//!
//! **The bound.** Slots never overlap: a reservation starts where the last one ended, and a refund gives back only the
//! last slot's unused end, never a slot others have queued behind ([`Bucket::refund`]). The bytes of each slot move
//! between [`BURST`] before its end and its end. So over any span `W`, a bucket lets through at most what its rate
//! moves in `W` + [`BURST`] + one slot, however many threads and Jobs share it; a slot is at most a quarter second at
//! the cap ([`piece`]), so over 5 s that is at most 5.35 s's worth, 7% over the cap. Nothing is leased to a thread
//! beyond the slot it holds, so nothing unspent adds up across threads. (A thread the scheduler holds back between
//! its turn and its I/O moves its slot's bytes late by that much.)
//!
//! **Uncapped.** A bucket with no rate is uncapped: [`Bucket::capped`] is one relaxed load, and nothing else touches
//! the bucket. Under `unthrottled` every bucket is uncapped, so the hot path costs the meter's counter and that load.
//!
//! **Pace changes.** [`Bucket::set_rate`] starts a new epoch: the queue is emptied and every receipt of the old epoch
//! is void. A thread waiting on one looks again at least every [`RECHECK`], and reserves afresh, at the new rate,
//! what of its slot was still to come, or goes at once if the bucket is uncapped. So a slower pace slows even a
//! single large file within a second, and a faster one, or none, ends a wait within [`RECHECK`]. Emptying the queue
//! lets at most one [`BURST`] more through for each change.
//!
//! **Waiting** sleeps, in slices of at most [`RECHECK`], and gives up when its `stop` says so: a cancelled Job's wait
//! ends within [`RECHECK`], and its slot is given back if it is still the last, or else left unused, so a cancel
//! leaves no debt for anyone. A wait holds no lock and nothing of the bucket but its receipt, so a panic or an early
//! return leaves at most that slot unused, and nothing ever waits for a waiter: a bucket wait can't be part of a
//! deadlock. It always ends, since a bucket's rate is never zero while capped and only the slots reserved before it
//! are ahead of it. Holding a Root's reader permit while waiting on its read bucket is intended: the permit's reader
//! is the one being held back. The store's writer is never held while waiting: a thread holding it reserves what it
//! reads and writes without waiting ([`crate::meter::NoReaderWaits`]). A 7z decoder waits on its read cap while it
//! holds its 7z gate permit, as it is the reader being held back; since every wait ends, the gate is held no longer
//! than the cap holds the decoder's reads.
//!
//! **Two caps at once.** A copy's chunk is paid for in two buckets, its source's reads and its destination's writes,
//! in one reservation ([`reserve_both`]): both queues are locked, always in the order of the buckets' addresses so
//! copies each way between two Roots can't deadlock, and the chunk gets a slot in each that ends at the same moment,
//! where the later queue's would. So its turn comes in both at once, it moves within [`BURST`] before both slots end,
//! and each bucket's bound holds as for reads; the earlier queue is left a gap before its slot, capacity unused, never
//! a burst. The copy then only waits, as a read does, and never gives up its place: however busy both queues are, it
//! waits only behind the slots reserved before it ([`take_both`]). A change of rate in either bucket voids the pair,
//! and starts the other bucket afresh too, once, since its queue holds the copies' slots at the old pace and a slot
//! can't be taken out of the middle of a queue: that lets through one [`BURST`] more there, as a change of its own
//! rate would.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// How far before its slot ends a reservation's bytes may move: what a bucket lets through at once after a pause, and
/// what spares a wait a sleep too short to keep.
pub const BURST: Duration = Duration::from_millis(100);
/// The longest a wait sleeps before it looks at the rate, and its stop, again.
pub const RECHECK: Duration = Duration::from_millis(100);

const NS: u128 = 1_000_000_000;

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// The nanoseconds `n` bytes take at `rate` bytes a second; at least one, so every slot has a length.
fn cost(n: u64, rate: u64) -> u64 {
    u64::try_from(u128::from(n) * NS / u128::from(rate.max(1)))
        .unwrap_or(u64::MAX)
        .max(1)
}

/// `part` of `whole` nanoseconds, of `n` bytes.
fn share(n: u64, part: u64, whole: u64) -> u64 {
    u64::try_from(u128::from(n) * u128::from(part) / u128::from(whole.max(1))).unwrap_or(u64::MAX)
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
    /// When, in [`Clock::now`]'s nanoseconds, the last slot reserved ends. Locked only for the arithmetic of a
    /// reservation or a refund, never across a wait.
    paid_until: Mutex<u64>,
    /// Moved on with each change of rate, and each [`Bucket::reset`], when the queue is emptied and every receipt before
    /// is void.
    epoch: AtomicU64,
    /// Moved on with each change of rate only.
    rates: AtomicU64,
    /// How many takes have waited, for tests.
    waits: AtomicU64,
}

/// A slot reserved in a [`Bucket`]'s queue for `n` bytes: they may move from [`BURST`] before its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receipt {
    /// The end of the queue before it: its start, unless it was reserved together with a busier queue's slot.
    prior: u64,
    start: u64,
    end: u64,
    n: u64,
    epoch: u64,
}

impl Receipt {
    /// When its bytes may move.
    fn turn(&self) -> u64 {
        self.end.saturating_sub(nanos(BURST))
    }

    /// The bytes it lets move: fewer than were asked for if a change of rate made them more than a [`piece`].
    pub fn bytes(&self) -> u64 {
        self.n
    }

    /// Whether its slot hasn't ended at `now`, in `bucket`'s epoch: whether bytes of it may still move.
    pub fn open(&self, bucket: &Bucket, now: u64) -> bool {
        now <= self.end && bucket.epoch() == self.epoch
    }

    /// How long to sleep at `now` before looking again, at most [`RECHECK`]; none once its turn has come, or once
    /// `bucket` is uncapped. A receipt a change of rate voided is replaced by one reserved now, at the new rate, for
    /// the share of its slot still to come, and for no more bytes than a [`piece`] at that rate: a slower pace never
    /// lets through at once a piece sized for a faster one.
    pub fn poll(&mut self, bucket: &Bucket, now: u64) -> Option<Duration> {
        if bucket.epoch() != self.epoch {
            let n = self.n.min(piece(bucket.rate()?) as u64);
            let rest = share(
                n,
                self.end.saturating_sub(now.max(self.start)),
                self.end - self.start,
            );
            *self = Receipt {
                n,
                ..bucket.reserve(rest, now)?
            };
        }
        let turn = self.turn();
        (turn > now).then(|| Duration::from_nanos(turn - now).min(RECHECK))
    }
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

    /// Caps the bucket at `rate` bytes a second, or uncaps it. Any change starts a new epoch, with the queue empty.
    pub fn set_rate(&self, rate: Option<u64>, _now: u64) {
        let rate = rate.unwrap_or(0);
        let mut paid = self.tail();
        if self.rate.swap(rate, Ordering::Relaxed) == rate {
            return;
        }
        *paid = 0;
        self.epoch.fetch_add(1, Ordering::Relaxed);
        self.rates.fetch_add(1, Ordering::Relaxed);
    }

    /// Empties the queue and starts a new epoch, if it is still in `epoch`, as a change of rate does: for slots a
    /// copy reserved together with another bucket's, at that bucket's pace, once that bucket's rate changed.
    fn reset(&self, epoch: u64) {
        let mut paid = self.tail();
        if self.epoch() == epoch {
            *paid = 0;
            self.epoch.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// How many times its rate has changed.
    fn rates(&self) -> u64 {
        self.rates.load(Ordering::Relaxed)
    }

    /// The end of its queue, locked: a reservation reads the rate and the epoch under it, so none straddles a change.
    fn tail(&self) -> MutexGuard<'_, u64> {
        self.paid_until
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The epoch reservations made now belong to.
    pub fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::Relaxed)
    }

    /// Reserves the next slot for `n` bytes at `now`: none while uncapped, or for no bytes, and then the bucket isn't
    /// touched.
    pub fn reserve(&self, n: u64, now: u64) -> Option<Receipt> {
        if n == 0 || !self.capped() {
            return None;
        }
        let mut paid = self.tail();
        let cost = cost(n, self.rate()?);
        let prior = *paid;
        let start = prior.max(now);
        let end = start.saturating_add(cost);
        *paid = end;
        Some(Receipt {
            prior,
            start,
            end,
            n,
            epoch: self.epoch(),
        })
    }

    /// Gives back, at `now`, `unused` bytes of `receipt` that didn't move, as by a read that found the end of its file
    /// first, from the end of its slot: only while its slot is still the last in the queue, and only what of it is
    /// still to come. Returns whether it was. The queue then ends where it did before the slot, or now if later, plus
    /// what is still to come of the part kept: so a joint slot given back gives back the gap left before it too. A
    /// slot others have queued behind is never given back, since the next reservation would then overlap theirs: its
    /// unused part is capacity lost, never a burst.
    pub fn refund(&self, receipt: &Receipt, unused: u64, now: u64) -> bool {
        if unused == 0 || self.epoch() != receipt.epoch {
            return false;
        }
        let len = receipt.end - receipt.start;
        let kept = receipt.start + len - share(unused.min(receipt.n), len, receipt.n);
        let back = receipt.prior.max(now) + kept.saturating_sub(now.max(receipt.start));
        let mut paid = self.tail();
        let last = back < receipt.end && *paid == receipt.end && self.epoch() == receipt.epoch;
        if last {
            *paid = back;
        }
        last
    }

    /// Reserves `n` bytes and waits, by `clock`, until their turn: the receipt, or none if uncapped; `None`, with the
    /// slot given back if it can be, if `stop` says to give up first. No more than the receipt's [`Receipt::bytes`]
    /// may move.
    pub fn take(
        &self,
        n: u64,
        clock: &(impl Clock + ?Sized),
        stop: &dyn Fn() -> bool,
    ) -> Option<Option<Receipt>> {
        let Some(mut receipt) = self.reserve(n, clock.now()) else {
            return Some(None);
        };
        let mut waited = false;
        while let Some(d) = receipt.poll(self, clock.now()) {
            if stop() {
                self.refund(&receipt, receipt.n, clock.now());
                return None;
            }
            if !waited {
                waited = true;
                self.waits.fetch_add(1, Ordering::Relaxed);
            }
            clock.sleep(d);
        }
        // A receipt of an epoch since uncapped is spent.
        Some(Some(receipt).filter(|r| r.epoch == self.epoch()))
    }

    /// How many takes have waited on it.
    pub fn waited(&self) -> u64 {
        self.waits.load(Ordering::Relaxed)
    }

    /// Whether nothing is reserved in it, since its rate last changed.
    pub fn untouched(&self) -> bool {
        *self.tail() == 0
    }
}

/// Reserves `n` bytes in both `a` and `b` at `now`, as one slot that ends at the same moment in each: none for an
/// uncapped bucket, and only one, in `a`, if they are the same bucket. Both queues are locked together, in the order
/// of their addresses, so two copies reserving the other way round can't deadlock, and no reservation in either comes
/// between. Each slot is its own bucket's cost of `n`, ending where the later of the two queues would; the other queue
/// is left a gap before it, capacity it doesn't use, never a burst.
pub fn reserve_both(a: &Bucket, b: &Bucket, n: u64, now: u64) -> [Option<Receipt>; 2] {
    if std::ptr::eq(a, b) {
        return [a.reserve(n, now), None];
    }
    if n == 0 {
        return [None, None];
    }
    let ordered = (a as *const Bucket) < (b as *const Bucket);
    let (first, second) = if ordered { (a, b) } else { (b, a) };
    let (mut first, mut second) = (first.tail(), second.tail());
    let (pa, pb) = if ordered {
        (&mut *first, &mut *second)
    } else {
        (&mut *second, &mut *first)
    };
    // Read under the locks, which a change of rate takes too: one bucket uncapped leaves the other a slot of its own.
    let receipt = |bucket: &Bucket, paid: &mut u64, end: Option<u64>| {
        let cost = cost(n, bucket.rate()?);
        let prior = *paid;
        let end = end.unwrap_or(prior.max(now).saturating_add(cost));
        *paid = end;
        Some(Receipt {
            prior,
            start: end - cost,
            end,
            n,
            epoch: bucket.epoch(),
        })
    };
    let end = match (a.rate(), b.rate()) {
        (Some(ra), Some(rb)) => Some(
            ((*pa).max(now).saturating_add(cost(n, ra)))
                .max((*pb).max(now).saturating_add(cost(n, rb))),
        ),
        _ => None,
    };
    [receipt(a, pa, end), receipt(b, pb, end)]
}

/// Reserves `n` bytes in both `a` and `b`, as a copy's chunk is in its source's read cap and its destination's write
/// cap, and waits, by `clock`, until their turn: their receipts, none for an uncapped bucket; `None` if `stop` says to
/// give up first, with each slot given back if it can be. No more than a receipt's [`Receipt::bytes`] may move: after
/// a change of rate, no more than a [`piece`] at the lower of the new rates.
///
/// The two slots are reserved together and end together ([`reserve_both`]), so the chunk's one turn comes in both
/// queues at once, and it moves within [`BURST`] before both slots end: each bucket lets it through just as it does a
/// read, and the bound of each holds as for reads. A copy never gives up a place it holds: it waits only behind the
/// slots reserved before its own in either queue, so neither readers nor other copies, however busy both queues, can
/// keep it out. A change of rate in either voids the pair: what of each is still to come is given back if it can be,
/// and both are reserved again together in the new epoch, for no more than a piece at the new rates, so an uncap
/// can't release a pile of slots whose turns have gone.
pub fn take_both(
    a: &Bucket,
    b: &Bucket,
    n: u64,
    clock: &(impl Clock + ?Sized),
    stop: &dyn Fn() -> bool,
) -> Option<[Option<Receipt>; 2]> {
    if n == 0 {
        return Some([None, None]);
    }
    let buckets = [a, b];
    let mut n = n;
    let reserve = |n: u64| {
        let now = clock.now();
        // The epochs first: a change after this voids the pair, one before it is in the receipts.
        let epochs = buckets.map(|b| (b.epoch(), b.rates()));
        (reserve_both(a, b, n, now), epochs)
    };
    let (mut held, mut epochs) = reserve(n);
    let mut waited = [false; 2];
    let give_back = |held: &[Option<Receipt>; 2], now: u64| {
        for (r, bucket) in held.iter().zip(buckets) {
            if let Some(r) = r {
                bucket.refund(r, r.n, now);
            }
        }
    };
    loop {
        let now = clock.now();
        if buckets.iter().zip(epochs).any(|(b, (e, _))| b.epoch() != e) {
            give_back(&held, now);
            // The other bucket's queue holds this copy's slot, and every waiting copy's like it, paced by the old
            // rate, and slots there can't be given back from the middle of the queue: it starts afresh too, rather
            // than every copy queue again behind slots of its own it can no longer use. Once for each change of rate,
            // at the epoch the copy saw, and never for a reset: one reset can't set off another.
            for (i, (bucket, (_, rates))) in buckets.iter().zip(epochs).enumerate() {
                let (other, (epoch, _)) = (buckets[1 - i], epochs[1 - i]);
                if bucket.rates() != rates && held[1 - i].is_some() {
                    other.reset(epoch);
                }
            }
            let most = buckets
                .iter()
                .filter_map(|b| b.rate())
                .map(|r| piece(r) as u64)
                .min();
            n = n.min(most.unwrap_or(n));
            (held, epochs) = reserve(n);
            continue;
        }
        let turn = held.iter().flatten().map(Receipt::turn).max();
        if turn.is_none_or(|t| t <= now) {
            return Some(held);
        }
        if stop() {
            give_back(&held, now);
            return None;
        }
        for (i, r) in held.iter().enumerate() {
            if r.is_some_and(|r| r.turn() > now) && !waited[i] {
                waited[i] = true;
                buckets[i].waits.fetch_add(1, Ordering::Relaxed);
            }
        }
        let turn = turn.unwrap_or(now);
        clock.sleep(Duration::from_nanos(turn - now).min(RECHECK));
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
        Waiting(usize, Receipt),
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
            let (i, receipt) = match e {
                Event::Rate(rate) => {
                    bucket.set_rate(rate, now);
                    continue;
                }
                Event::Ready(i) => (i, bucket.reserve(readers[i].chunk, now)),
                Event::Waiting(i, receipt) => (i, Some(receipt)),
            };
            // A reservation whose turn hasn't come looks again when its sleep ends; one whose turn has goes now.
            if let Some(mut receipt) = receipt
                && let Some(d) = receipt.poll(bucket, now)
            {
                queue.push(now + nanos(d), Event::Waiting(i, receipt));
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
        // A 64 MiB chunk at 16 MiB/s has the slot to 4 s, its turn at 3.9 s; the rate quadruples a second in. No more
        // than a piece at 64 MiB/s, 16 MiB, may then move, and the three quarters of its slot still to come are
        // reserved afresh for it: 12 MiB at 64 MiB/s, a slot to 1.1875 s.
        let faster = first(16, &[(SEC, Some(64 * MIB))]);
        assert_eq!(faster, 1_087_500_000);
        // At 64 MiB/s its slot ends at 1 s; the rate quarters half a second in. A piece at 16 MiB/s is 4 MiB, and half
        // of it is still to come: 2 MiB at 16 MiB/s, a slot to 625 ms.
        let slower = first(64, &[(SEC / 2, Some(16 * MIB))]);
        assert_eq!(slower, 525 * MS);
        // Uncapped while it waits, it goes within a recheck.
        let uncapped = first(1, &[(SEC, None)]);
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
        assert_eq!(bucket.reserve(64 * MIB, 5), None);
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
        assert!(bucket.take(10 * MIB, &clock, &|| false).is_some());
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
        assert!(bucket.take(100 * MIB, &clock, &|| stopped.get()).is_none());
        assert!(
            clock.now() <= 1_400 * MS + nanos(RECHECK),
            "{}",
            clock.now()
        );
        // Its slot, still the last, is given back: the next reservation owes nothing of it.
        assert!(!owes(&bucket, MIB, clock.now()));
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

    /// Whether `n` bytes reserved at `now` would have to wait.
    fn owes(bucket: &Bucket, n: u64, now: u64) -> bool {
        bucket
            .reserve(n, now)
            .is_some_and(|mut r| r.poll(bucket, now).is_some())
    }

    fn paid_until(bucket: &Bucket) -> u64 {
        *bucket.tail()
    }

    #[test]
    fn a_refund_gives_back_the_unused_end_of_the_last_slot() {
        let bucket = capped(10);
        // 10 MiB reserved for a chunk, of which the copy moved half a MiB.
        let r = bucket.reserve(10 * MIB, 0).unwrap();
        assert!(bucket.refund(&r, 19 * MIB / 2, 0));
        assert_eq!(paid_until(&bucket), 50 * MS);
        assert!(!owes(&bucket, MIB / 2, 0), "within the burst again");
    }

    #[test]
    fn a_refund_gives_back_only_time_still_to_come() {
        let bucket = capped(1);
        // 2 MiB at 1 MiB/s is paid for at 2 s; half a MiB of it is given back at 1.9 s, when only 0.1 s is still to
        // come.
        let r = bucket.reserve(2 * MIB, 0).unwrap();
        assert!(bucket.refund(&r, MIB / 2, 1_900 * MS));
        assert_eq!(paid_until(&bucket), 1_900 * MS);
        // Reserved before the rate changed, nothing is given back.
        bucket.set_rate(None, 2 * SEC);
        bucket.set_rate(Some(MIB), 2 * SEC);
        let later = bucket.reserve(MIB, 2 * SEC).unwrap();
        assert!(!bucket.refund(&r, MIB, 2 * SEC));
        assert_eq!(later.end, 3 * SEC);
        assert_eq!(paid_until(&bucket), 3 * SEC);
    }

    #[test]
    fn a_late_refund_never_gives_back_a_slot_others_queued_behind() {
        // Sixteen readers' 64 KiB reads at 1 MiB/s, each slot queued behind the one before. Each then finds the end of
        // its file, and gives its read back, in any order: only the last slot can be given back, so no reservation made
        // since overlaps one still held.
        let bucket = capped(1);
        let reads: Vec<_> = (0..16)
            .map(|_| bucket.reserve(64 << 10, 0).unwrap())
            .collect();
        assert_eq!(paid_until(&bucket), SEC);
        for r in &reads[..15] {
            assert!(!bucket.refund(r, 64 << 10, 0));
        }
        assert_eq!(
            paid_until(&bucket),
            SEC,
            "nothing given back from under the last slot"
        );
        assert!(bucket.refund(&reads[15], 64 << 10, 0));
        assert_eq!(paid_until(&bucket), reads[15].start);
        let next = bucket.reserve(64 << 10, 0).unwrap();
        assert_eq!(
            next.start, reads[14].end,
            "the next starts after every slot still held"
        );
    }

    #[test]
    fn uncapping_forgets_what_was_reserved_at_the_old_rate() {
        // 4 MiB reserved at 1 MiB/s, uncapped at 0.1 s, then capped at 100 MiB/s at 0.2 s: a MiB then fits the burst,
        // rather than waiting out the 3.7 s still owed at the old rate.
        let bucket = capped(1);
        let mut owed = bucket.reserve(4 * MIB, 0).unwrap();
        bucket.set_rate(None, 100 * MS);
        assert_eq!(
            owed.poll(&bucket, 100 * MS),
            None,
            "a wait from before ends"
        );
        bucket.set_rate(Some(100 * MIB), 200 * MS);
        assert!(!owes(&bucket, MIB, 200 * MS));
    }

    #[test]
    fn a_pace_change_reserves_afresh_only_what_was_still_to_come() {
        // 1.25 MiB at 10 MiB/s, its slot to 125 ms; at 25 ms the rate halves: the 1 MiB still to come takes 200 ms
        // from then, and the whole 1.25 MiB, still no more than a piece, moves at the end of it.
        let bucket = capped(10);
        let mut r = bucket.reserve(5 * MIB / 4, 0).unwrap();
        bucket.set_rate(Some(5 * MIB), 25 * MS);
        assert_eq!(r.poll(&bucket, 25 * MS), Some(RECHECK));
        assert_eq!((r.start, r.end, r.n), (25 * MS, 225 * MS, 5 * MIB / 4));
    }

    #[test]
    fn a_chunk_on_two_caps_moves_once_both_turns_have_come_and_before_either_slot_ends() {
        // A source at 4 MiB/s with 3 s already queued, a destination at 1 MiB/s with nothing: both slots are reserved
        // at once, each its own length, ending together where the source's queue would end. The destination's queue
        // is left a gap before its slot.
        let (a, b) = (capped(4), capped(1));
        a.reserve(12 * MIB, 0).unwrap();
        let clock = Fake::new();
        let receipts = take_both(&a, &b, MIB, &clock, &|| false).unwrap();
        let [Some(ra), Some(rb)] = receipts else {
            panic!("both capped")
        };
        let now = clock.now();
        assert_eq!((ra.start, ra.end), (3 * SEC, 3 * SEC + 250 * MS));
        assert_eq!((rb.start, rb.end), (2_250 * MS, 3 * SEC + 250 * MS));
        assert_eq!((ra.turn(), rb.turn()), (now, now));
        assert_eq!((paid_until(&a), paid_until(&b)), (ra.end, rb.end));
        assert!(clock.slept.borrow().iter().all(|d| *d <= RECHECK));
        // Uncapped, the destination isn't reserved at all.
        b.set_rate(None, now);
        let receipts = take_both(&a, &b, MIB, &clock, &|| false).unwrap();
        assert!(receipts[0].is_some() && receipts[1].is_none());
        // A zero-byte chunk reserves and waits for nothing.
        let waits = (a.waited(), paid_until(&a));
        assert_eq!(take_both(&a, &b, 0, &clock, &|| false), Some([None, None]));
        assert_eq!(a.take(0, &clock, &|| false), Some(None));
        assert_eq!((a.waited(), paid_until(&a)), waits);
    }

    #[test]
    fn a_cancelled_copy_gives_back_the_gap_its_joint_slot_left_in_the_faster_queue() {
        // A fast source at 100 MiB/s with nothing queued, a slow destination at 1 MiB/s with 30 s queued: the copy's
        // slot on the source is put 30 s out, behind a gap. Cancelled 100 ms in, it gives back both slots and the gap.
        let (a, b) = (capped(100), capped(1));
        b.reserve(30 * MIB, 0).unwrap();
        let clock = Fake::new();
        let stop = || clock.now() >= 100 * MS;
        assert_eq!(take_both(&a, &b, 256 << 10, &clock, &stop), None);
        let now = clock.now();
        assert_eq!(paid_until(&a), now);
        assert_eq!(paid_until(&b), 30 * SEC);
        // A read on the source alone, which nothing else is queued for, goes straight away.
        let waits = a.waited();
        let read = a.take(64 << 10, &clock, &|| false).unwrap().unwrap();
        assert_eq!(read.start, now);
        assert_eq!((a.waited(), clock.now()), (waits, now));
    }

    #[test]
    fn a_partly_used_joint_slot_keeps_only_what_is_still_to_come_of_its_used_part() {
        // A slot reserved with a gap before it, half of whose bytes were found unused before it began, keeps the
        // queue at the end before it plus the used half's cost; once it has begun, at now plus what's left of that.
        let (a, b) = (capped(4), capped(1));
        b.reserve(2 * MIB, 0).unwrap();
        let [Some(ra), Some(_)] = reserve_both(&a, &b, MIB, 0) else {
            panic!("both capped")
        };
        assert_eq!((ra.prior, ra.start, ra.end), (0, 2_750 * MS, 3 * SEC));
        assert!(a.refund(&ra, MIB / 2, SEC));
        assert_eq!(paid_until(&a), SEC + 125 * MS);
        let ra = a.reserve(MIB, 0).unwrap();
        assert_eq!((ra.prior, ra.start), (SEC + 125 * MS, SEC + 125 * MS));
        assert!(a.refund(&ra, MIB / 2, SEC + 200 * MS));
        assert_eq!(paid_until(&a), SEC + 250 * MS);
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

    #[test]
    fn a_slower_pace_lets_a_waiting_slot_move_no_more_than_a_piece_at_its_rate() {
        let bucket = Bucket::default();
        bucket.set_rate(Some(400 << 20), 0);
        let mut first = bucket.reserve(64 << 20, 0).unwrap();
        let mut waiting = bucket.reserve(64 << 20, 0).unwrap();
        bucket.set_rate(Some(1 << 20), MS);
        assert!(first.poll(&bucket, MS).is_some());
        assert!(waiting.poll(&bucket, MS).is_some());
        assert_eq!((first.bytes(), waiting.bytes()), (256 << 10, 256 << 10));
        assert!(
            paid_until(&bucket) <= MS + 500 * MS,
            "{} ms",
            paid_until(&bucket) / MS
        );
    }

    /// Runs `f` on a thread of its own, and fails if it hasn't ended within `secs` real seconds: a deadlock fails the
    /// test rather than hang it.
    fn within_real<T: Send + 'static>(secs: u64, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (done, ended) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(f()).unwrap());
        ended
            .recv_timeout(Duration::from_secs(secs))
            .expect("ended in time, without a deadlock")
    }

    #[test]
    fn a_copy_gets_its_turn_on_two_busy_caps_whose_queues_are_out_of_step() {
        // Both caps at 1 MiB/s, each kept busy by one reader of 256 KiB slots, 250 ms apart, the write queue's 125 ms
        // behind the read queue's: no 100 ms window of one ever meets one of the other. A copy reserving in both at
        // once still gets its turn behind the slots already queued.
        let (asked, took) = within_real(120, || {
            use lockstep::Lockstep;
            let clock = Lockstep::new();
            let caps = Arc::new(Caps::with_clock(clock.clone()));
            caps.set(Some(MIB), Some(MIB));
            let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            // Spawned from a thread on the clock, so that it doesn't move on before every one is.
            let (busy, copy) = clock
                .clone()
                .spawn(move || {
                    let busy = |write: bool, from: u64| {
                        let (clock, caps, done) = (clock.clone(), caps.clone(), done.clone());
                        clock.clone().spawn(move || {
                            clock.sleep(Duration::from_nanos(from));
                            let bucket = if write { &caps.write } else { &caps.read };
                            while !done.load(Ordering::Relaxed) && clock.now() < 60 * SEC {
                                bucket.take(256 << 10, &*clock, &|| false).unwrap();
                            }
                        })
                    };
                    let busy = [busy(false, 0), busy(true, 125 * MS)];
                    let copy = clock.clone().spawn(move || {
                        clock.sleep(Duration::from_secs(1));
                        let asked = clock.now();
                        let stop = || clock.now() >= 30 * SEC;
                        let receipts =
                            take_both(&caps.read, &caps.write, 256 << 10, &*clock, &stop);
                        let took = clock.now();
                        done.store(true, Ordering::Relaxed);
                        (receipts.is_some(), asked, took)
                    });
                    (busy, copy)
                })
                .join()
                .unwrap();
            let (got, asked, took) = copy.join().unwrap();
            for b in busy {
                b.join().unwrap();
            }
            assert!(got, "the copy never got its turn");
            (asked, took)
        });
        // Behind at most a slot already queued in each, and its own.
        assert!(took - asked <= 750 * MS, "{} ms", (took - asked) / MS);
    }

    #[test]
    fn copies_each_way_between_two_roots_all_get_their_turns_without_a_deadlock() {
        // Every reservation in two buckets at once locks both, in one order whichever way the copy goes.
        within_real(120, || {
            let (x, y) = (Arc::new(Bucket::default()), Arc::new(Bucket::default()));
            x.set_rate(Some(u64::MAX >> 20), 0);
            y.set_rate(Some(u64::MAX >> 20), 0);
            let threads: Vec<_> = (0..8)
                .map(|i| {
                    let (x, y) = (x.clone(), y.clone());
                    std::thread::spawn(move || {
                        for n in 0..20_000 {
                            let (a, b) = if (i + n) % 2 == 0 { (&x, &y) } else { (&y, &x) };
                            let [ra, rb] = reserve_both(a, b, 1, n);
                            assert_eq!(ra.unwrap().end, rb.unwrap().end);
                        }
                    })
                })
                .collect();
            for t in threads {
                t.join().unwrap();
            }
        });
        // And, waiting on the fake clock, copies each way at 1 MiB/s all finish, as fast as the caps allow.
        let ended = within_real(120, || {
            use lockstep::Lockstep;
            let clock = Lockstep::new();
            let (x, y) = (
                Arc::new(Caps::with_clock(clock.clone())),
                Arc::new(Caps::with_clock(clock.clone())),
            );
            x.set(Some(MIB), Some(MIB));
            y.set(Some(MIB), Some(MIB));
            let copies = clock
                .clone()
                .spawn(move || {
                    (0..8)
                        .map(|i| {
                            let (clock, x, y) = (clock.clone(), x.clone(), y.clone());
                            clock.clone().spawn(move || {
                                let (from, to) = if i % 2 == 0 { (&x, &y) } else { (&y, &x) };
                                let stop = || clock.now() >= 60 * SEC;
                                for _ in 0..4 {
                                    take_both(&from.read, &to.write, 256 << 10, &*clock, &stop)
                                        .expect("every copy gets its turn");
                                }
                                clock.now()
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .join()
                .unwrap();
            copies.into_iter().map(|c| c.join().unwrap()).max().unwrap()
        });
        // Each way moves 4 MiB through caps of 1 MiB/s: 4 s, less the burst.
        assert!(ended <= 4 * SEC, "{} ms", ended / MS);
    }
}
