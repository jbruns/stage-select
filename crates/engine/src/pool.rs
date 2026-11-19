//! A shared pool of permits whose size can change while it is used (ADR 0022): a Root's payload readers, the Host's
//! hash workers or its 7z archive workers, shared by every Job. A worker takes a permit before each file or payload
//! and gives it back when its [`Permit`] drops, so a smaller pool holds back the next file and stops nothing under
//! way.
//!
//! Taking a free permit while nobody waits is one compare-and-swap; nothing locks. Once anybody waits, workers queue
//! in order of arrival, so a Job that gives back a permit and asks again at once can't keep it from another Job that
//! waited first. A permit is given back on drop, so a worker that panics or returns early gives it back too.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

/// How often a waiting worker looks again even unwoken: for a pool made larger, and for a stop asked of it.
const RECHECK: Duration = Duration::from_millis(50);

/// Permits for at most `limit` workers at once.
#[derive(Debug)]
pub struct Pool {
    limit: AtomicUsize,
    inside: AtomicUsize,
    /// How many wait in `queue`, read without its lock.
    waiting: AtomicUsize,
    queue: Mutex<Queue>,
    freed: Condvar,
    most: AtomicUsize,
}

#[derive(Debug, Default)]
struct Queue {
    next: u64,
    tickets: VecDeque<u64>,
}

/// A worker's place in a [`Pool`], given back when dropped.
#[derive(Debug)]
#[must_use = "a permit is given back as soon as it drops"]
pub struct Permit<'p>(&'p Pool);

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// A [`Permit`] that holds its pool, for a guard that can't borrow it.
#[derive(Debug)]
#[must_use = "a permit is given back as soon as it drops"]
pub struct OwnedPermit(std::sync::Arc<Pool>);

impl Drop for OwnedPermit {
    fn drop(&mut self) {
        self.0.release();
    }
}

impl Pool {
    /// A pool of `limit` permits, at least one.
    pub fn new(limit: usize) -> Pool {
        Pool {
            limit: AtomicUsize::new(limit.max(1)),
            inside: AtomicUsize::new(0),
            waiting: AtomicUsize::new(0),
            queue: Mutex::default(),
            freed: Condvar::new(),
            most: AtomicUsize::new(0),
        }
    }

    /// Makes the pool `limit` permits, at least one, from the next permit taken. Workers already in stay in.
    pub fn set_limit(&self, limit: usize) {
        let was = self.limit.swap(limit.max(1), Ordering::SeqCst);
        if limit > was && self.waiting.load(Ordering::SeqCst) > 0 {
            self.wake();
        }
    }

    /// How many permits the pool has.
    pub fn limit(&self) -> usize {
        self.limit.load(Ordering::SeqCst)
    }

    /// How many permits are taken now.
    pub fn taken(&self) -> usize {
        self.inside.load(Ordering::SeqCst)
    }

    /// The most permits ever taken at once.
    pub fn most_at_once(&self) -> usize {
        self.most.load(Ordering::SeqCst)
    }

    /// Takes a permit if one is free, without waiting.
    fn try_take(&self) -> Option<Permit<'_>> {
        let limit = self.limit.load(Ordering::SeqCst);
        let mut inside = self.inside.load(Ordering::SeqCst);
        while inside < limit {
            match self.inside.compare_exchange_weak(
                inside,
                inside + 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    self.most.fetch_max(inside + 1, Ordering::Relaxed);
                    return Some(Permit(self));
                }
                Err(now) => inside = now,
            }
        }
        None
    }

    /// Waits for a permit of the pool `pool` and takes it.
    pub fn enter_owned(pool: &std::sync::Arc<Pool>) -> OwnedPermit {
        std::mem::forget(pool.enter());
        OwnedPermit(pool.clone())
    }

    fn release(&self) {
        self.inside.fetch_sub(1, Ordering::SeqCst);
        if self.waiting.load(Ordering::SeqCst) > 0 {
            self.wake();
        }
    }

    /// Waits for a permit and takes it.
    pub fn enter(&self) -> Permit<'_> {
        self.enter_unless(|| false)
            .expect("a wait that is never stopped ends with a permit")
    }

    /// Waits for a permit and takes it, unless `stop` says to give up first: then none. `stop` is asked about every
    /// [`RECHECK`] while waiting.
    pub fn enter_unless(&self, stop: impl Fn() -> bool) -> Option<Permit<'_>> {
        if self.waiting.load(Ordering::SeqCst) == 0
            && let Some(permit) = self.try_take()
        {
            return Some(permit);
        }
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let ticket = queue.next;
        queue.next += 1;
        queue.tickets.push_back(ticket);
        self.waiting.fetch_add(1, Ordering::SeqCst);
        // Leaves the queue however the wait ends, so a worker that stops waiting never holds up those behind it.
        let mut place = Place {
            pool: self,
            ticket,
            queue: Some(queue),
        };
        loop {
            let queue = place
                .queue
                .as_mut()
                .expect("the queue is held while waiting");
            if queue.tickets.front() == Some(&ticket)
                && let Some(permit) = self.try_take()
            {
                drop(place);
                return Some(permit);
            }
            if stop() {
                return None;
            }
            let held = place.queue.take().expect("the queue is held while waiting");
            let (held, _) = self
                .freed
                .wait_timeout(held, RECHECK)
                .unwrap_or_else(PoisonError::into_inner);
            place.queue = Some(held);
        }
    }

    fn wake(&self) {
        // Taken so that a worker between looking at the pool and waiting can't miss this.
        let _queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        self.freed.notify_all();
    }
}

/// A worker's ticket in a [`Pool`]'s queue, given up when dropped.
struct Place<'p> {
    pool: &'p Pool,
    ticket: u64,
    queue: Option<std::sync::MutexGuard<'p, Queue>>,
}

impl Drop for Place<'_> {
    fn drop(&mut self) {
        let mut queue = self.queue.take().unwrap_or_else(|| {
            self.pool
                .queue
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
        });
        queue.tickets.retain(|t| *t != self.ticket);
        self.pool.waiting.fetch_sub(1, Ordering::SeqCst);
        // The next in line may enter now.
        self.pool.freed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::channel;
    use std::sync::{Arc, Barrier};
    use std::time::Instant;

    use super::*;

    /// Runs `f` on a thread of its own, failing if it takes longer than 30 seconds: a permit stuck.
    fn bounded<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(30))
            .expect("it finished in time, with no permit stuck")
    }

    /// Waits until `n` workers wait in `pool`'s queue, failing after 30 seconds.
    fn until_waiting(pool: &Pool, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while pool.waiting.load(Ordering::SeqCst) != n {
            assert!(Instant::now() < deadline, "{n} never waited");
            std::thread::yield_now();
        }
    }

    #[test]
    fn a_pool_lets_in_as_many_as_its_limit_says_at_each_entry() {
        let pool = Pool::new(3);
        let all_in = Barrier::new(3);
        let permits = std::thread::scope(|s| {
            let workers: Vec<_> = (0..3)
                .map(|_| {
                    s.spawn(|| {
                        let permit = pool.enter();
                        all_in.wait();
                        permit
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|w| w.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(pool.most_at_once(), 3);

        // Made smaller, it keeps those already in, and lets in nobody more until fewer than its limit are left.
        pool.set_limit(1);
        let mut permits = permits.into_iter();
        for _ in 0..3 {
            assert!(pool.enter_unless(|| true).is_none(), "{} in", pool.taken());
            drop(permits.next());
        }
        assert!(pool.enter_unless(|| true).is_some());

        let (inside, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        std::thread::scope(|s| {
            for _ in 0..6 {
                s.spawn(|| {
                    for _ in 0..20 {
                        let _permit = pool.enter();
                        let now = inside.fetch_add(1, Ordering::SeqCst) + 1;
                        most.fetch_max(now, Ordering::SeqCst);
                        std::thread::yield_now();
                        inside.fetch_sub(1, Ordering::SeqCst);
                    }
                });
            }
        });
        assert_eq!(most.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_larger_pool_lets_in_those_waiting_without_a_permit_given_back() {
        let pool = Arc::new(Pool::new(1));
        let held = pool.enter();
        let waiter = {
            let pool = pool.clone();
            std::thread::spawn(move || {
                let _permit = pool.enter();
            })
        };
        until_waiting(&pool, 1);
        pool.set_limit(2);
        bounded(move || waiter.join().unwrap());
        drop(held);
    }

    #[test]
    fn a_permit_is_given_back_when_its_worker_panics() {
        let pool = Arc::new(Pool::new(1));
        let panicking = pool.clone();
        let panicked = std::thread::spawn(move || {
            let _permit = panicking.enter();
            panic!("a worker fails mid-file");
        })
        .join();
        assert!(panicked.is_err());
        assert_eq!(pool.taken(), 0);
        bounded(move || {
            let _permit = pool.enter();
        });
    }

    #[test]
    fn a_worker_that_stops_waiting_leaves_the_queue_and_holds_up_nobody() {
        let pool = Arc::new(Pool::new(1));
        let held = pool.enter();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = {
            let (pool, stop) = (pool.clone(), stop.clone());
            std::thread::spawn(move || {
                pool.enter_unless(|| stop.load(Ordering::SeqCst))
                    .map(|_| ())
            })
        };
        until_waiting(&pool, 1);
        // Queued behind the cancelled worker.
        let behind = {
            let pool = pool.clone();
            std::thread::spawn(move || {
                let _permit = pool.enter();
            })
        };
        until_waiting(&pool, 2);
        stop.store(true, Ordering::SeqCst);
        assert_eq!(cancelled.join().unwrap(), None);
        drop(held);
        bounded(move || behind.join().unwrap());
        assert_eq!(pool.taken(), 0);
    }

    #[test]
    fn a_worker_asking_again_at_once_cannot_keep_the_permit_from_one_that_waited() {
        let pool = Arc::new(Pool::new(1));
        let held = pool.enter();
        let (done, finish) = channel::<()>();
        let waiter = {
            let pool = pool.clone();
            std::thread::spawn(move || {
                let _permit = pool.enter();
                // Holds the permit, so that it isn't free again for asking once more.
                let _ = finish.recv();
            })
        };
        until_waiting(&pool, 1);
        // The moment the permit is given back, before the worker that waited has woken to take it.
        std::mem::forget(held);
        pool.inside.fetch_sub(1, Ordering::SeqCst);
        assert!(
            pool.enter_unless(|| true).is_none(),
            "asking again at once took the permit"
        );
        drop(done);
        bounded(move || waiter.join().unwrap());
    }
}
