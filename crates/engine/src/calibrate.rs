//! Calibration's sweeps (ADR 0022): how hash throughput on the Host, and read throughput from a Root, grow with the
//! number of workers, and the knee where adding more stops paying.
//!
//! A sweep runs steps of 1, 2, 4… workers. Each step stops at a time limit or a byte limit, whichever comes first, and
//! records the throughput it reached. The knee is the smallest count within 10% of the best throughput, never one past
//! a drop. A Root's sweep stops at a drop: more readers than that only contend.
//!
//! Every read is read-only, and each file is read at most once in a run, then dropped from the page cache so that
//! no later step reads it from memory.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Barrier, Mutex};
use std::time::{Duration, Instant};

use title_select_api::calibration::{CurvePoint, Knee};

use crate::meter::{JobMeter, Tally};
use crate::payload::Hasher;

/// The most payload readers a Root's sweep tries.
pub const MAX_READERS: usize = 32;

/// The throughput within which a smaller count is as good as the best: 10%.
const KNEE_SLACK: f64 = 0.9;

const CHUNK: usize = 1 << 16;

/// The synthetic payload each hash worker checksums over and over.
const HOST_PAYLOAD: usize = 16 << 20;

/// When each step stops, and how much a Root needs for a measurement worth having.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub step_time: Duration,
    pub step_bytes: u64,
    /// A Root whose files run out before the sweep has read this much is too small to measure; a step that ran out of
    /// files before reading a quarter of it measured too little to keep.
    pub min_bytes: u64,
    /// The most time a Root's step spends finding and opening files, outside the time it reads, before the sweep
    /// gives up on finding enough to read.
    pub find_time: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            step_time: Duration::from_secs(10),
            step_bytes: 2 << 30,
            min_bytes: 1 << 30,
            find_time: Duration::from_secs(60),
        }
    }
}

/// The time a step takes, measured from a fixed start. Tests drive a fake one from a throughput model.
pub trait Clock: Sync {
    fn elapsed(&self) -> Duration;
}

impl Clock for Instant {
    fn elapsed(&self) -> Duration {
        Instant::elapsed(self)
    }
}

/// The worker counts a sweep up to `max` tries: 1, 2, 4… and `max` itself.
pub fn counts(max: usize) -> Vec<usize> {
    let max = max.max(1);
    let mut counts: Vec<usize> = std::iter::successors(Some(1usize), |n| n.checked_mul(2))
        .take_while(|&n| n < max)
        .collect();
    counts.push(max);
    counts
}

/// Why a step stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepEnd {
    Time,
    Bytes,
    /// Its workers ran out of files to read.
    Exhausted,
    Cancelled,
    /// It couldn't find enough to read within [`Limits::find_time`], though there may be more.
    TimedOut,
}

/// What a step's workers share: the bytes they've got through, and whether to go on.
pub struct Budget<'a> {
    limits: Limits,
    clock: &'a dyn Clock,
    start: Duration,
    bytes: AtomicU64,
    cancel: &'a AtomicBool,
}

impl Budget<'_> {
    /// Whether the workers should take on more: not cancelled, and neither limit reached.
    pub fn going(&self) -> bool {
        !self.cancel.load(Ordering::Relaxed)
            && self.bytes.load(Ordering::Relaxed) < self.limits.step_bytes
            && self.clock.elapsed().saturating_sub(self.start) < self.limits.step_time
    }

    /// Counts `n` bytes got through.
    pub fn add(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }
}

/// One measured step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    pub point: CurvePoint,
    pub end: StepEnd,
}

/// Runs `workers` threads of `work` together until the step's limits, a cancel, or the work runs out, and measures
/// their throughput. Each `work` call takes on work while [`Budget::going`] says so, counting it with
/// [`Budget::add`], and returns when it stops or has nothing left.
pub fn step(
    workers: usize,
    limits: Limits,
    clock: &dyn Clock,
    cancel: &AtomicBool,
    work: &(dyn Fn(&Budget) + Sync),
) -> Step {
    let (bytes, took, end) = run(workers, limits, clock, cancel, work);
    Step {
        point: measured(workers, bytes, took),
        end,
    }
}

/// Runs a step's workers as [`step`] does: the bytes they got through, the time they took together, and why they
/// stopped.
fn run(
    workers: usize,
    limits: Limits,
    clock: &dyn Clock,
    cancel: &AtomicBool,
    work: &(dyn Fn(&Budget) + Sync),
) -> (u64, Duration, StepEnd) {
    let budget = Budget {
        limits,
        clock,
        start: clock.elapsed(),
        bytes: AtomicU64::new(0),
        cancel,
    };
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| work(&budget));
        }
    });
    let took = clock.elapsed().saturating_sub(budget.start);
    let bytes = budget.bytes.load(Ordering::Relaxed);
    let end = if cancel.load(Ordering::Relaxed) {
        StepEnd::Cancelled
    } else if bytes >= limits.step_bytes {
        StepEnd::Bytes
    } else if took >= limits.step_time {
        StepEnd::Time
    } else {
        StepEnd::Exhausted
    };
    (bytes, took, end)
}

fn measured(workers: usize, bytes: u64, took: Duration) -> CurvePoint {
    CurvePoint {
        workers,
        bytes,
        duration_ms: took.as_millis() as u64,
        bytes_per_s: bytes as f64 / took.as_secs_f64().max(1e-9),
    }
}

/// A sweep's curve and knee, or that it was cancelled before it finished.
#[derive(Debug, Clone, PartialEq)]
pub struct Sweep {
    pub curve: Vec<CurvePoint>,
    pub knee: Option<Knee>,
    pub cancelled: bool,
}

/// Runs `step` for each count in turn. It stops at a cancel, when the work runs out, and, when `stop_at_drop`, at the
/// first step slower than the one before. A step that ran out of work before reading `keep_short` bytes measured too
/// little and is left off the curve.
pub fn sweep(
    counts: &[usize],
    stop_at_drop: bool,
    keep_short: u64,
    mut step: impl FnMut(usize) -> Step,
) -> Sweep {
    let mut curve: Vec<CurvePoint> = Vec::new();
    let mut cancelled = false;
    for &count in counts {
        let Step { point, end } = step(count);
        match end {
            StepEnd::Cancelled => {
                cancelled = true;
                break;
            }
            StepEnd::TimedOut => break,
            StepEnd::Exhausted => {
                if point.bytes >= keep_short && point.bytes > 0 {
                    curve.push(point);
                }
                break;
            }
            StepEnd::Time | StepEnd::Bytes => {
                let dropped = curve
                    .last()
                    .is_some_and(|last| point.bytes_per_s < last.bytes_per_s);
                curve.push(point);
                if dropped && stop_at_drop {
                    break;
                }
            }
        }
    }
    Sweep {
        knee: knee(&curve),
        curve,
        cancelled,
    }
}

/// The smallest count within 10% of the best throughput, looking no further than the step before the first drop.
pub fn knee(curve: &[CurvePoint]) -> Option<Knee> {
    let before_drop = curve
        .windows(2)
        .position(|w| w[1].bytes_per_s < w[0].bytes_per_s)
        .map_or(curve.len(), |i| i + 1);
    let curve = &curve[..before_drop];
    let best = curve.iter().map(|p| p.bytes_per_s).fold(0.0, f64::max);
    curve
        .iter()
        .find(|p| p.bytes_per_s >= best * KNEE_SLACK)
        .map(|p| Knee {
            workers: p.workers,
            bytes_per_s: p.bytes_per_s,
        })
}

/// Sweeps hash workers 1, 2, 4… up to `cpus`, each checksumming synthetic data in memory with the Audit's hasher.
pub fn host(
    cpus: usize,
    limits: Limits,
    clock: &dyn Clock,
    cancel: &AtomicBool,
    job: Option<&JobMeter>,
) -> Sweep {
    let payload = synthetic(HOST_PAYLOAD);
    let work = |budget: &Budget| {
        let _active = job.map(JobMeter::hashing);
        while budget.going() {
            let mut hasher = Hasher::new();
            for chunk in payload.chunks(CHUNK) {
                if !budget.going() {
                    break;
                }
                hasher.update(chunk);
                budget.add(chunk.len() as u64);
            }
            std::hint::black_box(hasher.finish());
        }
    };
    sweep(&counts(cpus), false, 0, |n| {
        step(n, limits, clock, cancel, &work)
    })
}

/// Bytes that don't compress or repeat within a chunk, so that hashing them costs what hashing a real payload does.
fn synthetic(len: usize) -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.extend_from_slice(&x.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// A file a Root's sweep reads: opened, sized and dropped from the page cache before a step's clock starts, so that
/// the step times only reading.
pub trait Payload: Send {
    /// Its size, from the open handle.
    fn size(&self) -> std::io::Result<u64>;
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    /// Drops its pages from the page cache, where the platform can.
    fn uncache(&self);
}

impl Payload for File {
    fn size(&self) -> std::io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        Read::read(self, buf)
    }

    fn uncache(&self) {
        uncache(self);
    }
}

/// The files a Root's sweep reads, found as the sweep asks for them, each handed out at most once. Whoever implements
/// it opens each file beneath the Root without following a symlink, and only hands out regular files.
pub trait Files {
    type Payload: Payload;
    /// The next file to read; `None` once there are no more. An `Err` is a file, or a folder of files, that was
    /// found but couldn't be opened; the sweep goes on to the next.
    fn next(&self) -> Option<std::io::Result<Self::Payload>>;
}

/// The most files a step opens ahead of its readers: few enough to stay well inside the open-file limit.
const PREPARED_FILES: usize = 256;

/// How a Root's sweep went.
#[derive(Debug, Clone, PartialEq)]
pub enum RootSweep {
    /// It holds less than [`Limits::min_bytes`], these bytes, or its files ran out before any step measured enough
    /// to keep.
    TooSmall { read: u64 },
    /// It found nothing it could read: why the first file or folder it tried failed.
    Unreadable(String),
    /// It couldn't find enough to read in time for its first step, or to confirm it holds [`Limits::min_bytes`],
    /// having read these bytes, though the Root may hold more.
    TimedOut { read: u64 },
    /// Its curve, never empty unless cancelled, and the bytes it read in all.
    Swept { sweep: Sweep, read: u64 },
}

/// Sweeps payload readers 1, 2, 4… up to [`MAX_READERS`] over `files`, read-only, in the order they come. Each file
/// is read at most once across the sweep, so each step reads files no earlier step read.
///
/// Each step starts its readers once, before its clock, and keeps them across its batches. With its clock stopped, it
/// finds and opens up to [`PREPARED_FILES`] files, enough for what's left of its byte limit, and drops each from the
/// page cache; then its readers read them, timed from the first reader starting to the last finishing. It adds up
/// the bytes and the time of its reading across batches until it has read for its time limit or its byte limit, is
/// cancelled, or the Root has nothing left: its throughput is those bytes over that time. Files a batch finished are
/// dropped from the page cache again once its clock stops. A step that spends [`Limits::find_time`] finding files
/// without reaching a limit times out, and the sweep with it.
///
/// A file that can't be opened or read is passed over, and the first such failure kept: a Root none of whose data
/// could be read is unreadable.
///
/// A Root must hold at least [`Limits::min_bytes`] to be measured, whether or not its sweep read that much. When the
/// sweep read less, it counts the files it opened but didn't read and finds more, by size only, without reading them:
/// a Root whose files run out first is too small, and one it can't confirm within [`Limits::find_time`] times out.
pub fn root<F: Files + ?Sized>(
    files: &F,
    limits: Limits,
    clock: &dyn Clock,
    cancel: &AtomicBool,
    tally: &Tally,
) -> RootSweep {
    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }
    let read = AtomicU64::new(0);
    let failed = Mutex::new(None::<String>);
    let fail = |err: std::io::Error| {
        lock(&failed).get_or_insert_with(|| err.to_string());
    };
    let prepared: Mutex<VecDeque<(F::Payload, u64)>> = Mutex::new(VecDeque::new());
    let finished: Mutex<Vec<F::Payload>> = Mutex::new(Vec::new());
    // Finds and opens files for `workers` readers to read `wanted` bytes, until `until` on the clock.
    let prepare = |workers: usize, wanted: u64, until: Duration, ran_out: &mut bool| {
        // What each reader of an even share would read of each file.
        let share = (wanted / workers as u64).max(1);
        let mut queue = lock(&prepared);
        let mut have: u64 = queue.iter().map(|(_, size)| (*size).min(share)).sum();
        while !*ran_out
            && (have < wanted || queue.len() < workers)
            && queue.len() < PREPARED_FILES
            && !cancel.load(Ordering::Relaxed)
            && clock.elapsed() < until
        {
            match files.next() {
                None => *ran_out = true,
                Some(Err(err)) => fail(err),
                Some(Ok(payload)) => match payload.size() {
                    Err(err) => fail(err),
                    Ok(0) => {}
                    Ok(size) => {
                        payload.uncache();
                        have += size.min(share);
                        queue.push_back((payload, size));
                    }
                },
            }
        }
    };
    let nanos = |d: Duration| d.as_nanos().min(u64::MAX as u128) as u64;
    let mut ran_out = false;
    let mut timed_out = false;
    let sweep = sweep(&counts(MAX_READERS), true, limits.min_bytes / 4, |n| {
        let batch = Batch::default();
        // The readers start before the step is timed, and wait at `go` for each batch and at `done` after it.
        let go = Barrier::new(n + 1);
        let done = Barrier::new(n + 1);
        let going = || {
            !cancel.load(Ordering::Relaxed)
                && batch.bytes.load(Ordering::Relaxed) < limits.step_bytes
                && batch.took.load(Ordering::Relaxed)
                    + nanos(clock.elapsed()).saturating_sub(batch.start.load(Ordering::Relaxed))
                    < nanos(limits.step_time)
        };
        let reader = || {
            let _active = tally.job().map(JobMeter::reading);
            let mut buf = vec![0; CHUNK];
            loop {
                go.wait();
                if batch.stop.load(Ordering::Relaxed) {
                    return;
                }
                batch
                    .first
                    .fetch_min(nanos(clock.elapsed()), Ordering::Relaxed);
                'files: loop {
                    if !going() {
                        batch.cut.store(true, Ordering::Relaxed);
                        break;
                    }
                    let Some((mut payload, _)) = lock(&prepared).pop_front() else {
                        break;
                    };
                    loop {
                        if !going() {
                            batch.cut.store(true, Ordering::Relaxed);
                            tally.file_read();
                            lock(&finished).push(payload);
                            break 'files;
                        }
                        match payload.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                batch.bytes.fetch_add(n as u64, Ordering::Relaxed);
                                read.fetch_add(n as u64, Ordering::Relaxed);
                                tally.read(n);
                            }
                            Err(err) => {
                                fail(err);
                                break;
                            }
                        }
                    }
                    tally.file_read();
                    lock(&finished).push(payload);
                }
                batch
                    .last
                    .fetch_max(nanos(clock.elapsed()), Ordering::Relaxed);
                done.wait();
            }
        };
        let end = std::thread::scope(|s| {
            for _ in 0..n {
                s.spawn(reader);
            }
            let mut finding = Duration::ZERO;
            let end = loop {
                if cancel.load(Ordering::Relaxed) {
                    break StepEnd::Cancelled;
                }
                let start = clock.elapsed();
                let until = start + limits.find_time.saturating_sub(finding);
                let wanted = limits
                    .step_bytes
                    .saturating_sub(batch.bytes.load(Ordering::Relaxed));
                prepare(n, wanted, until, &mut ran_out);
                finding += clock.elapsed().saturating_sub(start);
                if lock(&prepared).is_empty() {
                    break if cancel.load(Ordering::Relaxed) {
                        StepEnd::Cancelled
                    } else if ran_out {
                        StepEnd::Exhausted
                    } else {
                        timed_out = true;
                        StepEnd::TimedOut
                    };
                }
                batch.first.store(u64::MAX, Ordering::Relaxed);
                batch.last.store(0, Ordering::Relaxed);
                batch.start.store(nanos(clock.elapsed()), Ordering::Relaxed);
                go.wait();
                done.wait();
                // Only the time the readers spent reading: from the first to start to the last to stop.
                let first = batch.first.load(Ordering::Relaxed);
                let last = batch.last.load(Ordering::Relaxed);
                batch
                    .took
                    .fetch_add(last.saturating_sub(first), Ordering::Relaxed);
                for payload in lock(&finished).drain(..) {
                    payload.uncache();
                }
                if cancel.load(Ordering::Relaxed) {
                    break StepEnd::Cancelled;
                }
                if batch.bytes.load(Ordering::Relaxed) >= limits.step_bytes {
                    break StepEnd::Bytes;
                }
                if batch.cut.load(Ordering::Relaxed) {
                    break StepEnd::Time;
                }
            };
            batch.stop.store(true, Ordering::Relaxed);
            go.wait();
            end
        });
        Step {
            point: measured(
                n,
                batch.bytes.into_inner(),
                Duration::from_nanos(batch.took.into_inner()),
            ),
            end,
        }
    });
    let mut sweep = sweep;
    let read = read.into_inner();
    if sweep.cancelled {
        return RootSweep::Swept { sweep, read };
    }
    if read == 0
        && let Some(why) = lock(&failed).clone()
    {
        return RootSweep::Unreadable(why);
    }
    if read < limits.min_bytes {
        // Too small is how much the Root holds, not how much a slow sweep read: what the sweep read, what it opened
        // and didn't read, then the sizes of more files, found without reading them, until there is enough.
        let mut held = read + lock(&prepared).iter().map(|(_, size)| size).sum::<u64>();
        let until = clock.elapsed() + limits.find_time;
        while held < limits.min_bytes
            && !ran_out
            && !cancel.load(Ordering::Relaxed)
            && clock.elapsed() < until
        {
            match files.next() {
                None => ran_out = true,
                Some(Err(err)) => fail(err),
                Some(Ok(payload)) => match payload.size() {
                    Err(err) => fail(err),
                    Ok(size) => held += size,
                },
            }
        }
        if cancel.load(Ordering::Relaxed) {
            sweep.cancelled = true;
            return RootSweep::Swept { sweep, read };
        }
        if held < limits.min_bytes {
            return if ran_out {
                RootSweep::TooSmall { read: held }
            } else {
                RootSweep::TimedOut { read }
            };
        }
    }
    if sweep.curve.is_empty() {
        return if timed_out {
            RootSweep::TimedOut { read }
        } else {
            RootSweep::TooSmall { read }
        };
    }
    RootSweep::Swept { sweep, read }
}

/// What a Root's step and its readers share across its batches, in nanoseconds on the step's clock.
#[derive(Default)]
struct Batch {
    /// The bytes the step has read.
    bytes: AtomicU64,
    /// The time the step's earlier batches spent reading.
    took: AtomicU64,
    /// When this batch started, to stop at the step's time limit.
    start: AtomicU64,
    /// When the first reader started this batch, and the last stopped.
    first: AtomicU64,
    last: AtomicU64,
    /// Whether a reader stopped at a limit with files left.
    cut: AtomicBool,
    /// Whether the readers are done for the step.
    stop: AtomicBool,
}

/// Drops `file`'s pages from the page cache, so that what was read isn't read again from memory, nor was read from
/// memory in the first place. Where the platform has no such advice, it does nothing.
fn uncache(file: &File) {
    #[cfg(target_os = "linux")]
    let _ = rustix::fs::fadvise(file, 0, None, rustix::fs::Advice::DontNeed);
    #[cfg(not(target_os = "linux"))]
    let _ = file;
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;

    /// A clock that only moves when the fake work says so.
    #[derive(Default)]
    struct Fake(AtomicU64);

    impl Fake {
        fn advance(&self, by: Duration) {
            self.0.fetch_add(by.as_nanos() as u64, Ordering::Relaxed);
        }
    }

    impl Clock for Fake {
        fn elapsed(&self) -> Duration {
            Duration::from_nanos(self.0.load(Ordering::Relaxed))
        }
    }

    /// Work whose `workers` together get through `rate(workers)` bytes a second, a MiB at a time.
    fn modelled<'a>(
        clock: &'a Fake,
        workers: usize,
        rate: &'a (dyn Fn(usize) -> f64 + Sync),
    ) -> impl Fn(&Budget) + Sync + 'a {
        move |budget: &Budget| {
            while budget.going() {
                clock.advance(Duration::from_secs_f64(MIB as f64 / rate(workers)));
                budget.add(MIB);
            }
        }
    }

    fn point(workers: usize, mib_per_s: f64) -> CurvePoint {
        CurvePoint {
            workers,
            bytes: 0,
            duration_ms: 0,
            bytes_per_s: mib_per_s,
        }
    }

    fn timed(point: CurvePoint) -> Step {
        Step {
            point,
            end: StepEnd::Time,
        }
    }

    #[test]
    fn counts_double_up_to_the_most() {
        assert_eq!(counts(32), [1, 2, 4, 8, 16, 32]);
        assert_eq!(counts(12), [1, 2, 4, 8, 12]);
        assert_eq!(counts(1), [1]);
        assert_eq!(counts(0), [1]);
    }

    #[test]
    fn the_knee_is_the_smallest_count_within_ten_percent_of_the_best() {
        let curve = [
            point(1, 100.0),
            point(2, 180.0),
            point(4, 300.0),
            point(8, 320.0),
            point(16, 330.0),
        ];
        let knee = knee(&curve).unwrap();
        assert_eq!(knee.workers, 4, "300 is within 10% of 330; 180 isn't");
        assert_eq!(knee.bytes_per_s, 300.0);
    }

    #[test]
    fn the_knee_is_never_one_past_a_drop() {
        // The best is after the drop, but a count past a drop is never the knee.
        let curve = [
            point(1, 100.0),
            point(2, 200.0),
            point(4, 150.0),
            point(8, 400.0),
        ];
        assert_eq!(knee(&curve).unwrap().workers, 2);
        assert_eq!(knee(&[]), None);
    }

    #[test]
    fn a_root_sweep_stops_at_a_drop_and_the_host_sweep_does_not() {
        let model =
            |n: usize| [100.0, 200.0, 150.0, 400.0, 500.0, 600.0][n.trailing_zeros() as usize];
        let mut tried = Vec::new();
        let swept = sweep(&counts(32), true, 0, |n| {
            tried.push(n);
            timed(point(n, model(n)))
        });
        assert_eq!(tried, [1, 2, 4], "the step that dropped is the last");
        assert_eq!(swept.curve.len(), 3);
        assert_eq!(swept.knee.unwrap().workers, 2);
        assert!(!swept.cancelled);

        let swept = sweep(&counts(32), false, 0, |n| timed(point(n, model(n))));
        assert_eq!(swept.curve.len(), 6);
        assert_eq!(swept.knee.unwrap().workers, 2);
    }

    #[test]
    fn a_step_stops_at_its_time_limit_when_that_comes_first() {
        let clock = Fake::default();
        let rate = |_| 100.0 * MIB as f64;
        let step = step(
            1,
            Limits::default(),
            &clock,
            &AtomicBool::new(false),
            &modelled(&clock, 1, &rate),
        );
        assert_eq!(step.end, StepEnd::Time);
        assert_eq!(step.point.bytes, 1000 * MIB, "10 s at 100 MiB/s");
        assert_eq!(step.point.duration_ms, 10_000);
        assert!((step.point.bytes_per_s - 100.0 * MIB as f64).abs() < 1.0);
    }

    #[test]
    fn a_step_stops_at_its_byte_limit_when_that_comes_first() {
        let clock = Fake::default();
        let rate = |workers: usize| workers as f64 * 1024.0 * MIB as f64;
        let step = step(
            4,
            Limits::default(),
            &clock,
            &AtomicBool::new(false),
            &modelled(&clock, 4, &rate),
        );
        assert_eq!(step.end, StepEnd::Bytes);
        // Each worker may take one more MiB between the check and the count.
        assert!((2048 * MIB..2048 * MIB + 4 * MIB).contains(&step.point.bytes));
        assert!(step.point.duration_ms <= 1000, "2 GiB at 4 GiB/s");
    }

    #[test]
    fn a_cancelled_step_ends_the_sweep_with_what_it_finished() {
        let cancel = AtomicBool::new(false);
        let swept = sweep(&counts(8), false, 0, |n| {
            if n == 4 {
                cancel.store(true, Ordering::Relaxed);
            }
            let clock = Fake::default();
            let rate = |n: usize| n as f64 * MIB as f64;
            step(
                n,
                Limits::default(),
                &clock,
                &cancel,
                &modelled(&clock, n, &rate),
            )
        });
        assert!(swept.cancelled);
        assert_eq!(
            swept.curve.iter().map(|p| p.workers).collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn a_short_step_that_ran_out_of_files_is_left_off() {
        let swept = sweep(&counts(8), true, 10, |n| Step {
            point: CurvePoint {
                bytes: if n == 1 { 100 } else { 5 },
                ..point(n, n as f64)
            },
            end: if n == 1 {
                StepEnd::Bytes
            } else {
                StepEnd::Exhausted
            },
        });
        assert_eq!(swept.curve.len(), 1);
        assert!(!swept.cancelled);
    }

    /// Files handed out in order, each at most once: `Ok(size)` a file of that many bytes, `Err` one that can't be
    /// opened.
    struct Listed {
        dir: tempfile::TempDir,
        next: std::sync::Mutex<std::vec::IntoIter<(usize, Result<usize, ()>)>>,
    }

    impl Listed {
        fn new(sizes: &[Result<usize, ()>]) -> Listed {
            let dir = tempfile::tempdir().unwrap();
            let next = sizes
                .iter()
                .enumerate()
                .map(|(i, &size)| {
                    if let Ok(size) = size {
                        std::fs::write(dir.path().join(format!("{i}")), vec![i as u8; size])
                            .unwrap();
                    }
                    (i, size)
                })
                .collect::<Vec<_>>()
                .into_iter();
            Listed {
                dir,
                next: std::sync::Mutex::new(next),
            }
        }
    }

    impl Files for Listed {
        type Payload = File;

        fn next(&self) -> Option<std::io::Result<File>> {
            let (i, size) = self.next.lock().unwrap().next()?;
            Some(match size {
                Ok(_) => File::open(self.dir.path().join(format!("{i}"))),
                Err(()) => Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("{i}: permission denied"),
                )),
            })
        }
    }

    fn run(files: &Listed, limits: Limits) -> RootSweep {
        root(
            files,
            limits,
            &Instant::now(),
            &AtomicBool::new(false),
            &Tally::default(),
        )
    }

    #[test]
    fn a_root_whose_files_run_out_before_the_minimum_is_too_small_to_measure() {
        let files = Listed::new(&[Ok(1000), Ok(1000)]);
        let limits = Limits {
            min_bytes: 2001,
            ..Limits::default()
        };
        assert_eq!(run(&files, limits), RootSweep::TooSmall { read: 2000 });
    }

    #[test]
    fn a_root_sweep_reads_each_file_at_most_once() {
        let files = Listed::new(&[Ok(4096); 64]);
        let limits = Limits {
            step_time: Duration::from_secs(60),
            step_bytes: 8 * 4096,
            min_bytes: 4096,
            ..Limits::default()
        };
        let RootSweep::Swept { sweep, read } = run(&files, limits) else {
            panic!("big enough");
        };
        assert!(
            read <= 64 * 4096,
            "read {read} bytes of {}: a file read twice",
            64 * 4096
        );
        assert_eq!(read, sweep.curve.iter().map(|p| p.bytes).sum::<u64>());
    }

    #[test]
    fn a_few_huge_files_first_leave_files_for_every_later_step() {
        // Each huge file is far more than a whole sweep reads of it, but a step reads only its limit and moves on:
        // the files after them are still there for the later steps.
        let mut sizes = vec![Ok(64 * 4096); 2];
        sizes.extend([Ok(4096); 256]);
        let files = Listed::new(&sizes);
        let limits = Limits {
            step_time: Duration::from_secs(60),
            step_bytes: 8 * 4096,
            min_bytes: 4096,
            ..Limits::default()
        };
        let RootSweep::Swept { sweep, .. } = run(&files, limits) else {
            panic!("big enough");
        };
        assert!(
            sweep.curve.len() > 1,
            "only {:?} measured",
            sweep.curve.iter().map(|p| p.workers).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_root_none_of_whose_files_can_be_opened_is_unreadable() {
        let files = Listed::new(&[Err(()), Err(())]);
        assert_eq!(
            run(&files, Limits::default()),
            RootSweep::Unreadable("0: permission denied".into())
        );
    }

    #[test]
    fn files_that_cannot_be_opened_are_passed_over() {
        let files = Listed::new(&[Err(()), Ok(1000), Err(()), Ok(1000)]);
        let limits = Limits {
            min_bytes: 2000,
            ..Limits::default()
        };
        let RootSweep::Swept { sweep, read } = run(&files, limits) else {
            panic!("the readable files are enough");
        };
        assert_eq!(read, 2000);
        assert!(!sweep.curve.is_empty());
    }

    #[test]
    fn a_root_with_nothing_to_read_is_never_measured() {
        let files = Listed::new(&[]);
        let limits = Limits {
            min_bytes: 0,
            ..Limits::default()
        };
        assert_eq!(run(&files, limits), RootSweep::TooSmall { read: 0 });
    }

    /// A Root's throughput on the fake clock: a millisecond a MiB whatever the number of readers, and twice that once
    /// `slow_after` bytes have been read. It keeps the threads that read.
    #[derive(Default)]
    struct Model {
        clock: Fake,
        read: AtomicU64,
        slow_after: Option<u64>,
        threads: std::sync::Mutex<std::collections::HashSet<std::thread::ThreadId>>,
    }

    struct Modelled<'a> {
        left: u64,
        model: &'a Model,
    }

    impl Payload for Modelled<'_> {
        fn size(&self) -> std::io::Result<u64> {
            Ok(self.left)
        }

        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let model = self.model;
            model
                .threads
                .lock()
                .unwrap()
                .insert(std::thread::current().id());
            let n = (buf.len() as u64).min(self.left);
            self.left -= n;
            let before = model.read.fetch_add(n, Ordering::Relaxed);
            let slow = model.slow_after.is_some_and(|after| before >= after);
            let per_mib = if slow { 2_000_000 } else { 1_000_000 };
            model.clock.advance(Duration::from_nanos(n * per_mib / MIB));
            Ok(n as usize)
        }

        fn uncache(&self) {
            self.model.clock.advance(Duration::from_millis(10));
        }
    }

    /// Files of these sizes, each taking `find` on the fake clock to find and open.
    struct Fakes<'a> {
        model: &'a Model,
        find: Duration,
        sizes: std::sync::Mutex<std::vec::IntoIter<u64>>,
    }

    impl<'a> Fakes<'a> {
        fn new(model: &'a Model, find: Duration, sizes: Vec<u64>) -> Self {
            Fakes {
                model,
                find,
                sizes: std::sync::Mutex::new(sizes.into_iter()),
            }
        }
    }

    impl<'a> Files for Fakes<'a> {
        type Payload = Modelled<'a>;

        fn next(&self) -> Option<std::io::Result<Modelled<'a>>> {
            let left = self.sizes.lock().unwrap().next()?;
            self.model.clock.advance(self.find);
            Some(Ok(Modelled {
                left,
                model: self.model,
            }))
        }
    }

    fn modelled_root(files: &Fakes, limits: Limits) -> RootSweep {
        root(
            files,
            limits,
            &files.model.clock,
            &AtomicBool::new(false),
            &Tally::default(),
        )
    }

    #[test]
    fn finding_and_opening_files_is_not_timed_as_reading() {
        let model = Model::default();
        let files = Fakes::new(&model, Duration::from_millis(200), vec![MIB; 1000]);
        let limits = Limits {
            step_time: Duration::from_secs(60),
            step_bytes: 32 * MIB,
            min_bytes: MIB,
            ..Limits::default()
        };
        let RootSweep::Swept { sweep, .. } = modelled_root(&files, limits) else {
            panic!("big enough");
        };
        assert_eq!(
            sweep.curve.iter().map(|p| p.workers).collect::<Vec<_>>(),
            counts(MAX_READERS),
            "no drop"
        );
        for p in &sweep.curve {
            assert!(
                (p.bytes_per_s - 1000.0 * MIB as f64).abs() < 1.0,
                "{} readers read at {} MiB/s",
                p.workers,
                p.bytes_per_s / MIB as f64
            );
        }
    }

    #[test]
    fn many_tiny_files_short_of_the_minimum_are_too_small_to_measure() {
        let model = Model::default();
        let files = Fakes::new(&model, Duration::ZERO, vec![4096; 2048]);
        assert_eq!(
            modelled_root(&files, Limits::default()),
            RootSweep::TooSmall { read: 2048 * 4096 }
        );
    }

    #[test]
    fn small_files_enough_in_all_fill_every_step_batch_after_batch() {
        let model = Model::default();
        let files = Fakes::new(&model, Duration::ZERO, vec![4096; 4096]);
        let limits = Limits {
            step_time: Duration::from_secs(60),
            step_bytes: 2 * MIB,
            min_bytes: 4 * MIB,
            ..Limits::default()
        };
        let RootSweep::Swept { sweep, .. } = modelled_root(&files, limits) else {
            panic!("big enough");
        };
        assert_eq!(
            sweep.curve.iter().map(|p| p.workers).collect::<Vec<_>>(),
            counts(MAX_READERS)
        );
        for p in &sweep.curve {
            assert!(
                p.bytes >= limits.step_bytes,
                "{} readers read only {} bytes",
                p.workers,
                p.bytes
            );
        }
    }

    #[test]
    fn empty_files_slow_to_find_before_the_data_are_passed_over() {
        let model = Model::default();
        let mut sizes = vec![0; 20];
        sizes.extend([MIB; 100]);
        let files = Fakes::new(&model, Duration::from_secs(1), sizes);
        let limits = Limits {
            step_time: Duration::from_secs(10),
            step_bytes: 8 * MIB,
            min_bytes: MIB,
            find_time: Duration::from_secs(60),
        };
        let RootSweep::Swept { sweep, .. } = modelled_root(&files, limits) else {
            panic!("the data after the empty files is enough");
        };
        assert!(sweep.curve.len() > 1, "{:?}", sweep.curve);
    }

    #[test]
    fn a_root_with_nothing_found_to_read_in_time_times_out_rather_than_being_too_small() {
        let model = Model::default();
        let mut sizes = vec![0; 1000];
        sizes.extend([MIB; 100]);
        let files = Fakes::new(&model, Duration::from_secs(1), sizes);
        let limits = Limits {
            step_time: Duration::from_secs(10),
            step_bytes: 8 * MIB,
            min_bytes: MIB,
            find_time: Duration::from_secs(30),
        };
        assert_eq!(
            modelled_root(&files, limits),
            RootSweep::TimedOut { read: 0 }
        );
    }

    #[test]
    fn each_step_starts_its_readers_once_however_many_batches_it_reads() {
        let model = Model::default();
        // 4 KiB files: each step reads its 2 MiB over two batches of 256 or more.
        let files = Fakes::new(&model, Duration::ZERO, vec![4096; 4096]);
        let limits = Limits {
            step_time: Duration::from_secs(60),
            step_bytes: 2 * MIB,
            min_bytes: 4 * MIB,
            ..Limits::default()
        };
        let RootSweep::Swept { sweep, .. } = modelled_root(&files, limits) else {
            panic!("big enough");
        };
        assert_eq!(sweep.curve.len(), counts(MAX_READERS).len());
        let started: usize = counts(MAX_READERS).iter().sum();
        let threads = model.threads.lock().unwrap().len();
        assert!(
            threads <= started,
            "{threads} reader threads read, for {started} readers"
        );
    }

    /// A root of `mib` MiB in 1 MiB files, reading at half the speed after its first 200 MiB: the second step drops.
    fn dropping(model: &mut Model, mib: usize) -> Vec<u64> {
        model.slow_after = Some(200 * MIB);
        vec![MIB; mib]
    }

    fn dropping_limits() -> Limits {
        Limits {
            step_time: Duration::from_secs(600),
            step_bytes: 200 * MIB,
            ..Limits::default()
        }
    }

    #[test]
    fn a_root_under_the_minimum_whose_sweep_drops_early_is_too_small() {
        let mut model = Model::default();
        let sizes = dropping(&mut model, 768);
        let files = Fakes::new(&model, Duration::ZERO, sizes);
        assert_eq!(
            modelled_root(&files, dropping_limits()),
            RootSweep::TooSmall { read: 768 * MIB }
        );
    }

    #[test]
    fn a_large_root_whose_sweep_drops_before_reading_the_minimum_is_measured() {
        let mut model = Model::default();
        let sizes = dropping(&mut model, 2048);
        let files = Fakes::new(&model, Duration::ZERO, sizes);
        let RootSweep::Swept { sweep, read } = modelled_root(&files, dropping_limits()) else {
            panic!("it holds 2 GiB");
        };
        assert_eq!(
            sweep.curve.iter().map(|p| p.workers).collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(read < Limits::default().min_bytes);
        assert_eq!(sweep.knee.map(|k| k.workers), Some(1));
    }

    #[test]
    fn a_root_under_the_minimum_whose_later_step_times_out_is_too_small_or_timed_out() {
        let limits = Limits {
            step_time: Duration::from_secs(600),
            step_bytes: 20 * MIB,
            find_time: Duration::from_secs(30),
            ..Limits::default()
        };
        // 25 MiB, then empty files, each slow to find: the first step reads 20 MiB, and the second times out finding
        // more.
        let root_of = |empties: usize| {
            let model = Model::default();
            let mut sizes = vec![MIB; 25];
            sizes.extend(std::iter::repeat_n(0, empties));
            let files = Fakes::new(&model, Duration::from_secs(1), sizes);
            modelled_root(&files, limits)
        };
        assert_eq!(
            root_of(40),
            RootSweep::TooSmall { read: 25 * MIB },
            "the files ran out"
        );
        assert!(
            matches!(root_of(1000), RootSweep::TimedOut { .. }),
            "more files to find"
        );
    }

    #[test]
    fn a_cancel_while_files_are_found_ends_the_sweep() {
        struct Cancelling<'a>(&'a AtomicBool);
        impl Files for Cancelling<'_> {
            type Payload = File;
            fn next(&self) -> Option<std::io::Result<File>> {
                self.0.store(true, Ordering::Relaxed);
                Some(Err(std::io::Error::other("never reached")))
            }
        }
        let cancel = AtomicBool::new(false);
        let RootSweep::Swept { sweep, read } = root(
            &Cancelling(&cancel),
            Limits::default(),
            &Instant::now(),
            &cancel,
            &Tally::default(),
        ) else {
            panic!("cancelled");
        };
        assert!(sweep.cancelled && sweep.curve.is_empty());
        assert_eq!(read, 0);
    }

    #[test]
    fn the_host_sweep_measures_every_count_up_to_the_cpus() {
        let limits = Limits {
            step_time: Duration::from_secs(5),
            step_bytes: 4 * MIB,
            min_bytes: 0,
            ..Limits::default()
        };
        let swept = host(3, limits, &Instant::now(), &AtomicBool::new(false), None);
        assert_eq!(
            swept.curve.iter().map(|p| p.workers).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert!(swept.curve.iter().all(|p| p.bytes >= 4 * MIB));
        assert!(swept.knee.is_some());
    }
}
